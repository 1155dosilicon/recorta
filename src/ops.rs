//! Processamento de imagem: abrir, salvar, recortar, redimensionar/escalar.
//!
//! Recorte, giro e espelhamento são sem perdas (só copiam pixels).
//! Redimensionar usa `fast_image_resize` (SIMD), com alfa pré-multiplicado
//! para não criar bordas escuras em áreas transparentes.

use std::io::Cursor;
use std::path::Path;

use fast_image_resize as fr;
use image::metadata::Orientation;
use image::{ColorType, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgba32FImage};
use rayon::prelude::*;

/// Extensões que o seletor de arquivos oferece para abrir.
pub const EXTENSOES_ABRIR: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "tga", "ico", "qoi", "pnm", "pbm",
    "pgm", "ppm", "pam", "dds", "exr", "hdr", "ff",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metodo {
    Lanczos3,
    CatmullRom,
    Mitchell,
    Bilinear,
    Vizinho,
    PixelArt,
}

impl Metodo {
    pub const TODOS: [Metodo; 6] = [
        Metodo::Lanczos3,
        Metodo::CatmullRom,
        Metodo::Mitchell,
        Metodo::Bilinear,
        Metodo::Vizinho,
        Metodo::PixelArt,
    ];

    pub fn nome(self) -> &'static str {
        match self {
            Metodo::Lanczos3 => "Lanczos3 (fotos, padrão)",
            Metodo::CatmullRom => "Catmull-Rom (mais nítido)",
            Metodo::Mitchell => "Mitchell (mais suave)",
            Metodo::Bilinear => "Bilinear (rápido)",
            Metodo::Vizinho => "Vizinho mais próximo (pixels)",
            Metodo::PixelArt => "Pixel art suavizado (Scale2x/3x)",
        }
    }

    pub fn dica(self) -> &'static str {
        match self {
            Metodo::Lanczos3 => {
                "Melhor equilíbrio entre nitidez e suavidade. Bom para fotos, ampliando ou reduzindo."
            }
            Metodo::CatmullRom => "Bicúbico nítido, com menos halo que o Lanczos.",
            Metodo::Mitchell => "Bicúbico suave: menos serrilhado e halo, um pouco mais borrado.",
            Metodo::Bilinear => "Rápido, porém mais borrado ao ampliar.",
            Metodo::Vizinho => {
                "Cada pixel vira um bloco. Ideal para pixel art e ícones pequenos ampliados em fator inteiro (2x, 3x, 4x…)."
            }
            Metodo::PixelArt => {
                "Amplia pixel art arredondando as diagonais (algoritmos Scale2x/Scale3x) sem borrar as cores."
            }
        }
    }

    fn alg(self) -> fr::ResizeAlg {
        use fr::{FilterType as F, ResizeAlg as A};
        match self {
            Metodo::Lanczos3 => A::Convolution(F::Lanczos3),
            Metodo::CatmullRom => A::Convolution(F::CatmullRom),
            Metodo::Mitchell => A::Convolution(F::Mitchell),
            Metodo::Bilinear => A::Convolution(F::Bilinear),
            Metodo::Vizinho => A::Nearest,
            Metodo::PixelArt => A::Convolution(F::CatmullRom),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Opcoes {
    pub metodo: Metodo,
    /// Reamostrar em luz linear (cores e brilho mais corretos, mais lento).
    pub linear: bool,
    /// Máscara de nitidez depois de reamostrar (0 = desligada).
    pub nitidez: f32,
}

impl Default for Opcoes {
    fn default() -> Self {
        Opcoes { metodo: Metodo::Lanczos3, linear: false, nitidez: 0.0 }
    }
}

// ---------------------------------------------------------------------------
// Abrir / salvar

pub fn abrir(caminho: &Path) -> Result<DynamicImage, String> {
    let mut leitor = ImageReader::open(caminho)
        .map_err(|e| format!("não consegui abrir: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("não consegui ler: {e}"))?;
    leitor.no_limits();
    let mut dec = leitor.into_decoder().map_err(|e| format!("formato não suportado: {e}"))?;
    let orientacao = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| format!("imagem inválida: {e}"))?;
    // fotos de celular guardam a rotação no EXIF; aplica para mostrar "em pé"
    img.apply_orientation(orientacao);
    Ok(img)
}

/// Salva conforme a extensão. Escreve num temporário e renomeia, para nunca
/// estragar o arquivo original se a codificação falhar no meio.
pub fn salvar(img: &DynamicImage, caminho: &Path, qualidade_jpeg: u8) -> Result<(), String> {
    let formato = ImageFormat::from_path(caminho)
        .map_err(|_| "extensão desconhecida (use .png, .jpg, .webp…)".to_string())?;
    let bytes = codificar(img, formato, qualidade_jpeg)?;
    // se o destino é um link simbólico, grava no arquivo para onde ele aponta
    // (o rename trocaria o link por um arquivo comum)
    let destino = std::fs::canonicalize(caminho).unwrap_or_else(|_| caminho.to_path_buf());
    let tmp = destino.with_extension(format!(
        "{}.recorta-tmp",
        destino.extension().and_then(|e| e.to_str()).unwrap_or("img")
    ));
    let gravar = || -> std::io::Result<()> {
        std::fs::write(&tmp, &bytes)?;
        // mantém as permissões do arquivo que está sendo substituído
        if let Ok(meta) = std::fs::metadata(&destino) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &destino)
    };
    gravar().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("não consegui gravar: {e}")
    })
}

fn codificar(img: &DynamicImage, formato: ImageFormat, qualidade_jpeg: u8) -> Result<Vec<u8>, String> {
    let mut saida = Cursor::new(Vec::new());
    if formato == ImageFormat::Jpeg {
        let rgb = achatar_em_branco(img);
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut saida, qualidade_jpeg.clamp(1, 100));
        rgb.write_with_encoder(enc).map_err(|e| e.to_string())?;
        return Ok(saida.into_inner());
    }
    if formato == ImageFormat::Ico && (img.width() > 256 || img.height() > 256) {
        return Err("ICO aceita no máximo 256×256 px".into());
    }
    match img.write_to(&mut saida, formato) {
        Ok(()) => Ok(saida.into_inner()),
        // formato não aceita esse tipo de pixel (ex.: 16 bits em BMP): converte p/ 8 bits
        Err(_) => {
            let convertida = if img.color().has_alpha() {
                DynamicImage::ImageRgba8(img.to_rgba8())
            } else {
                DynamicImage::ImageRgb8(img.to_rgb8())
            };
            let mut saida = Cursor::new(Vec::new());
            convertida.write_to(&mut saida, formato).map_err(|e| e.to_string())?;
            Ok(saida.into_inner())
        }
    }
}

/// JPEG não tem transparência: compõe sobre branco.
fn achatar_em_branco(img: &DynamicImage) -> DynamicImage {
    if !img.color().has_alpha() {
        return DynamicImage::ImageRgb8(img.to_rgb8());
    }
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (d, s) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = s[3] as u32;
        for c in 0..3 {
            d[c] = ((s[c] as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        }
    }
    DynamicImage::ImageRgb8(rgb)
}

// ---------------------------------------------------------------------------
// Redimensionar

/// Reamostra para exatamente `largura`×`altura`. Se a proporção mudar, a
/// imagem estica/achata (isso é "redimensionar"); "escala" é só chamar com
/// as duas medidas multiplicadas pelo mesmo fator.
pub fn redimensionar(img: &DynamicImage, largura: u32, altura: u32, op: Opcoes) -> Result<DynamicImage, String> {
    if largura == 0 || altura == 0 {
        return Err("tamanho precisa ser pelo menos 1×1".into());
    }
    let tipo = img.color();
    let fator = (largura as f32 / img.width() as f32).max(altura as f32 / img.height() as f32);

    let mut saida = match op.metodo {
        Metodo::PixelArt => pixel_art(img, largura, altura)?,
        Metodo::Vizinho => reamostrar(img, largura, altura, Metodo::Vizinho.alg())?,
        m if op.linear => {
            let mut f = img.to_rgba32f();
            mapear_rgb(&mut f, srgb_para_linear);
            let mut r = reamostrar(&DynamicImage::ImageRgba32F(f), largura, altura, m.alg())?.into_rgba32f();
            mapear_rgb(&mut r, linear_para_srgb);
            DynamicImage::ImageRgba32F(r)
        }
        m => reamostrar(img, largura, altura, m.alg())?,
    };

    if op.nitidez > 0.0 && !matches!(op.metodo, Metodo::Vizinho | Metodo::PixelArt) {
        // raio proporcional à ampliação: ao ampliar 25x, o "borrado" tem ~25 px
        let sigma = (0.6 * fator.max(1.0)).clamp(0.7, 12.0);
        saida = nitidez(&saida, op.nitidez, sigma);
    }
    Ok(para_tipo(saida, tipo))
}

fn reamostrar(img: &DynamicImage, largura: u32, altura: u32, alg: fr::ResizeAlg) -> Result<DynamicImage, String> {
    let opcoes = fr::ResizeOptions::new().resize_alg(alg);
    let mut resizer = fr::Resizer::new();
    let mut destino = DynamicImage::new(largura, altura, img.color());
    match resizer.resize(img, &mut destino, &opcoes) {
        Ok(()) => Ok(destino),
        Err(_) => {
            // tipo de pixel que a biblioteca não aceita: passa por RGBA 32F
            let src = DynamicImage::ImageRgba32F(img.to_rgba32f());
            let mut destino = DynamicImage::new(largura, altura, ColorType::Rgba32F);
            resizer.resize(&src, &mut destino, &opcoes).map_err(|e| e.to_string())?;
            Ok(destino)
        }
    }
}

/// Converte de volta para o tipo de pixel original (8/16 bits, cinza, etc.).
fn para_tipo(img: DynamicImage, tipo: ColorType) -> DynamicImage {
    if img.color() == tipo {
        return img;
    }
    match tipo {
        ColorType::L8 => DynamicImage::ImageLuma8(img.to_luma8()),
        ColorType::La8 => DynamicImage::ImageLumaA8(img.to_luma_alpha8()),
        ColorType::Rgb8 => DynamicImage::ImageRgb8(img.to_rgb8()),
        ColorType::Rgba8 => DynamicImage::ImageRgba8(img.to_rgba8()),
        ColorType::L16 => DynamicImage::ImageLuma16(img.to_luma16()),
        ColorType::La16 => DynamicImage::ImageLumaA16(img.to_luma_alpha16()),
        ColorType::Rgb16 => DynamicImage::ImageRgb16(img.to_rgb16()),
        ColorType::Rgba16 => DynamicImage::ImageRgba16(img.to_rgba16()),
        ColorType::Rgb32F => DynamicImage::ImageRgb32F(img.to_rgb32f()),
        _ => img,
    }
}

fn srgb_para_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_para_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn mapear_rgb(img: &mut Rgba32FImage, f: fn(f32) -> f32) {
    img.par_chunks_mut(4).for_each(|p| {
        p[0] = f(p[0]);
        p[1] = f(p[1]);
        p[2] = f(p[2]);
    });
}

/// Máscara de nitidez (unsharp mask): realça o que o filtro suavizou.
///
/// O desfoque é feito com alfa pré-multiplicado: o RGB "escondido" dos pixels
/// transparentes (geralmente preto) não pode vazar para a borda do que é
/// visível, senão aparece um halo claro/escuro em volta de recortes e ícones.
fn nitidez(img: &DynamicImage, quantidade: f32, sigma: f32) -> DynamicImage {
    let orig = img.to_rgba32f();
    let mut pre = orig.clone();
    pre.par_chunks_mut(4).for_each(|p| {
        for c in 0..3 {
            p[c] *= p[3];
        }
    });
    let borrada = image::imageops::fast_blur(&pre, sigma);
    let mut saida = orig.clone();
    saida
        .par_chunks_mut(4)
        .zip(orig.par_chunks(4).zip(borrada.par_chunks(4)))
        .for_each(|(d, (o, b))| {
            if b[3] <= f32::EPSILON {
                return; // vizinhança toda transparente: nada a realçar
            }
            for c in 0..3 {
                let media = b[c] / b[3];
                d[c] = (o[c] + quantidade * (o[c] - media)).clamp(0.0, 1.0);
            }
        });
    DynamicImage::ImageRgba32F(saida)
}

// ---------------------------------------------------------------------------
// Pixel art: Scale2x / Scale3x (EPX)

fn pixel_art(img: &DynamicImage, largura: u32, altura: u32) -> Result<DynamicImage, String> {
    let mut atual = img.to_rgba8();
    // pixels totalmente transparentes contam como iguais, seja qual for o RGB
    for p in atual.pixels_mut() {
        if p[3] == 0 {
            *p = image::Rgba([0, 0, 0, 0]);
        }
    }
    loop {
        let r = (largura as f64 / atual.width() as f64).max(altura as f64 / atual.height() as f64);
        let inteiro3 = (r / 3.0 - (r / 3.0).round()).abs() < 1e-6 && (r / 2.0).fract() != 0.0;
        if r >= 3.0 && (r < 4.0 || inteiro3) {
            atual = scale3x(&atual);
        } else if r >= 1.5 {
            atual = scale2x(&atual);
        } else {
            break;
        }
    }
    let img = DynamicImage::ImageRgba8(atual);
    if img.width() == largura && img.height() == altura {
        Ok(img)
    } else {
        // ajuste final (normalmente reduzindo um pouco a partir do tamanho maior)
        reamostrar(&img, largura, altura, Metodo::CatmullRom.alg())
    }
}

type Px = [u8; 4];

fn vizinhos(src: &image::RgbaImage) -> impl Fn(i64, i64) -> Px + '_ {
    let (w, h) = (src.width() as i64, src.height() as i64);
    move |x, y| src.get_pixel(x.clamp(0, w - 1) as u32, y.clamp(0, h - 1) as u32).0
}

fn scale2x(src: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = src.dimensions();
    let px = vizinhos(src);
    let mut out = image::RgbaImage::new(w * 2, h * 2);
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let p = px(x, y);
            let a = px(x, y - 1);
            let b = px(x + 1, y);
            let c = px(x - 1, y);
            let d = px(x, y + 1);
            let e0 = if c == a && c != d && a != b { a } else { p };
            let e1 = if a == b && a != c && b != d { b } else { p };
            let e2 = if d == c && d != b && c != a { c } else { p };
            let e3 = if b == d && b != a && d != c { d } else { p };
            let (ox, oy) = (x as u32 * 2, y as u32 * 2);
            out.put_pixel(ox, oy, image::Rgba(e0));
            out.put_pixel(ox + 1, oy, image::Rgba(e1));
            out.put_pixel(ox, oy + 1, image::Rgba(e2));
            out.put_pixel(ox + 1, oy + 1, image::Rgba(e3));
        }
    }
    out
}

fn scale3x(src: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = src.dimensions();
    let px = vizinhos(src);
    let mut out = image::RgbaImage::new(w * 3, h * 3);
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (a, b, c) = (px(x - 1, y - 1), px(x, y - 1), px(x + 1, y - 1));
            let (d, e, f) = (px(x - 1, y), px(x, y), px(x + 1, y));
            let (g, hh, i) = (px(x - 1, y + 1), px(x, y + 1), px(x + 1, y + 1));
            let mut o = [e; 9];
            if b != hh && d != f {
                o[0] = if d == b { d } else { e };
                o[1] = if (d == b && e != c) || (b == f && e != a) { b } else { e };
                o[2] = if b == f { f } else { e };
                o[3] = if (d == b && e != g) || (d == hh && e != a) { d } else { e };
                o[5] = if (b == f && e != i) || (hh == f && e != c) { f } else { e };
                o[6] = if d == hh { d } else { e };
                o[7] = if (d == hh && e != i) || (hh == f && e != g) { hh } else { e };
                o[8] = if hh == f { f } else { e };
            }
            for (k, v) in o.iter().enumerate() {
                out.put_pixel(x as u32 * 3 + (k % 3) as u32, y as u32 * 3 + (k / 3) as u32, image::Rgba(*v));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod testes {
    use super::*;

    fn xadrez(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
            if (x + y) % 2 == 0 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 128]) }
        }))
    }

    #[test]
    fn redimensiona_para_tamanho_exato_e_mantem_tipo() {
        let img = xadrez(20, 20);
        for m in Metodo::TODOS {
            for linear in [false, true] {
                let op = Opcoes { metodo: m, linear, nitidez: 0.5 };
                let r = redimensionar(&img, 500, 500, op).unwrap();
                assert_eq!((r.width(), r.height()), (500, 500), "{m:?}");
                assert_eq!(r.color(), ColorType::Rgba8);
                let r = redimensionar(&img, 200, 10, op).unwrap();
                assert_eq!((r.width(), r.height()), (200, 10), "{m:?}");
            }
        }
    }

    #[test]
    fn mantem_16_bits() {
        let img = DynamicImage::ImageRgb16(image::ImageBuffer::from_pixel(10, 10, image::Rgb([1000u16, 2000, 60000])));
        let r = redimensionar(&img, 37, 5, Opcoes::default()).unwrap();
        assert_eq!(r.color(), ColorType::Rgb16);
        assert_eq!(r.as_rgb16().unwrap().get_pixel(3, 3).0, [1000, 2000, 60000]);
    }

    #[test]
    fn scale2x_arredonda_diagonal() {
        // diagonal preta em fundo branco
        let mut img = image::RgbaImage::from_pixel(3, 3, image::Rgba([255; 4]));
        for i in 0..3 {
            img.put_pixel(i, i, image::Rgba([0, 0, 0, 255]));
        }
        let r = scale2x(&img);
        assert_eq!(r.dimensions(), (6, 6));
        // vizinho do pixel (1,1) acima-direita fica branco; o canto junto à diagonal fica preto
        assert_eq!(r.get_pixel(2, 1).0, [0, 0, 0, 255]);
        assert_eq!(scale3x(&img).dimensions(), (9, 9));
    }

    #[test]
    fn nitidez_nao_cria_halo_na_borda_transparente() {
        // cinza opaco à esquerda, transparente (RGB preto) à direita
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(40, 10, |x, _| {
            if x < 20 { image::Rgba([128, 128, 128, 255]) } else { image::Rgba([0, 0, 0, 0]) }
        }));
        let r = nitidez(&img, 1.5, 3.0).to_rgba8();
        for x in 0..20 {
            let p = r.get_pixel(x, 5).0;
            assert!(p[0].abs_diff(128) <= 1, "x={x}: {p:?}");
            assert_eq!(p[3], 255);
        }
    }

    #[cfg(unix)]
    #[test]
    fn salvar_mantem_link_simbolico_e_permissoes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("recorta-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.png");
        let link = dir.join("link.png");
        salvar(&xadrez(4, 4), &real, 90).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        salvar(&xadrez(8, 3), &link, 90).unwrap();

        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(abrir(&real).unwrap().width(), 8);
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o640);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn salva_e_reabre() {
        let dir = std::env::temp_dir().join(format!("recorta-teste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = xadrez(16, 9);
        for ext in ["png", "jpg", "webp", "bmp", "tiff", "gif", "qoi", "tga"] {
            let p = dir.join(format!("t.{ext}"));
            salvar(&img, &p, 90).unwrap_or_else(|e| panic!("{ext}: {e}"));
            let r = abrir(&p).unwrap();
            assert_eq!((r.width(), r.height()), (16, 9), "{ext}");
        }
        // PNG é sem perdas
        let r = abrir(&dir.join("t.png")).unwrap();
        assert_eq!(r.to_rgba8(), img.to_rgba8());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
