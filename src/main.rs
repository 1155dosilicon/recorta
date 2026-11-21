//! Recorta — editor simples de imagens: recortar com proporções, redimensionar
//! (esticar/achatar) e mudar a escala com reamostragem de qualidade.

// no Windows, sem a janela preta de console atrás do programa
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ops;

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, CursorIcon, Key, KeyboardShortcut, Modifiers, Pos2,
    Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use image::DynamicImage;
use ops::{Metodo, Opcoes};

const LIMITE_DESFAZER_BYTES: usize = 1536 * 1024 * 1024;
const LIMITE_MEGAPIXELS: f64 = 400.0;

fn main() -> eframe::Result {
    let arg = std::env::args_os().nth(1);
    if arg.as_deref().is_some_and(|a| a == "-h" || a == "--help") {
        println!("uso: recorta [imagem]\n\nEditor de imagens: recortar, redimensionar e escalar.");
        return Ok(());
    }
    let arquivo = arg.map(PathBuf::from);

    let mut viewport = egui::ViewportBuilder::default()
        // Wayland: liga a janela ao recorta.desktop. No X11 vira WM_CLASS ("", "recorta"):
        // o eframe sempre define um app_id e o egui-winit deixa a instância vazia; os
        // painéis casam pela classe, que bate com o StartupWMClass do atalho.
        .with_app_id("recorta")
        .with_title("Recorta")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([820.0, 520.0]);
    if let Ok(icone) = image::load_from_memory(include_bytes!("../assets/recorta-64.png")) {
        let icone = icone.to_rgba8();
        viewport = viewport.with_icon(egui::IconData {
            width: icone.width(),
            height: icone.height(),
            rgba: icone.into_raw(),
        });
    }
    let forcado = std::env::var(VAR_RENDERER).ok();
    let renderer = match forcado.as_deref() {
        Some("glow") => eframe::Renderer::Glow,
        #[cfg(windows)]
        Some("wgpu") => eframe::Renderer::Wgpu,
        _ => RENDERER_PADRAO,
    };
    let opcoes = eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    };
    let resultado = eframe::run_native(
        "recorta",
        opcoes,
        Box::new(move |cc| Ok(Box::new(App::new(cc, arquivo)))),
    );

    if let Err(erro) = &resultado {
        // A janela não abriu (driver de vídeo sem suporte ao backend). O winit não deixa
        // recriar o loop de eventos no mesmo processo, então reabre o programa com OpenGL.
        #[cfg(windows)]
        if forcado.is_none()
            && let Ok(exe) = std::env::current_exe()
            && let Ok(status) = std::process::Command::new(exe)
                .args(std::env::args_os().skip(1))
                .env(VAR_RENDERER, "glow")
                .status()
        {
            std::process::exit(status.code().unwrap_or(1));
        }
        let msg = format!(
            "O Recorta não conseguiu abrir a janela:\n\n{erro}\n\n\
             Verifique se o driver de vídeo está instalado (é preciso OpenGL 2.0 ou mais novo)."
        );
        eprintln!("{msg}");
        // no Windows não há console (windows_subsystem); mostra uma caixa de mensagem
        #[cfg(windows)]
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Recorta")
            .set_description(msg)
            .show();
    }
    resultado
}

/// Variável de ambiente para forçar o backend gráfico: `glow` (OpenGL) ou `wgpu` (só Windows).
const VAR_RENDERER: &str = "RECORTA_RENDERER";
/// Windows: wgpu (DirectX 12, com fallback por software). Linux: OpenGL, que funciona
/// até com drivers Mesa antigos.
#[cfg(windows)]
const RENDERER_PADRAO: eframe::Renderer = eframe::Renderer::Wgpu;
#[cfg(not(windows))]
const RENDERER_PADRAO: eframe::Renderer = eframe::Renderer::Glow;

// ---------------------------------------------------------------------------
// Estado

struct Doc {
    img: Arc<DynamicImage>,
    caminho: Option<PathBuf>,
    desfazer: Vec<(Arc<DynamicImage>, String)>,
    refazer: Vec<(Arc<DynamicImage>, String)>,
    modificado: bool,
}

impl Doc {
    fn nome(&self) -> String {
        self.caminho
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sem nome".into())
    }
    fn tam(&self) -> (u32, u32) {
        (self.img.width(), self.img.height())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ferramenta {
    Recortar,
    Redimensionar,
    Escala,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Proporcao {
    Livre,
    Original,
    Fixa(u32, u32),
    Personalizada,
}

const PROPORCOES: &[(&str, Proporcao)] = &[
    ("Livre", Proporcao::Livre),
    ("Original da imagem", Proporcao::Original),
    ("1:1 (quadrado)", Proporcao::Fixa(1, 1)),
    ("4:3", Proporcao::Fixa(4, 3)),
    ("3:4", Proporcao::Fixa(3, 4)),
    ("16:9", Proporcao::Fixa(16, 9)),
    ("9:16 (stories)", Proporcao::Fixa(9, 16)),
    ("3:2", Proporcao::Fixa(3, 2)),
    ("2:3", Proporcao::Fixa(2, 3)),
    ("5:4", Proporcao::Fixa(5, 4)),
    ("4:5 (retrato)", Proporcao::Fixa(4, 5)),
    ("16:10", Proporcao::Fixa(16, 10)),
    ("21:9 (ultrawide)", Proporcao::Fixa(21, 9)),
    ("2:1", Proporcao::Fixa(2, 1)),
    ("Personalizada…", Proporcao::Personalizada),
];

const TAMANHOS_COMUNS: &[(&str, u32, u32)] = &[
    ("3840 × 2160 (4K)", 3840, 2160),
    ("2560 × 1440 (QHD)", 2560, 1440),
    ("1920 × 1080 (Full HD)", 1920, 1080),
    ("1366 × 768", 1366, 768),
    ("1280 × 720 (HD)", 1280, 720),
    ("1080 × 1920 (vertical)", 1080, 1920),
    ("1080 × 1080 (quadrado)", 1080, 1080),
    ("1024 × 768", 1024, 768),
    ("800 × 600", 800, 600),
    ("512 × 512", 512, 512),
    ("256 × 256 (ícone)", 256, 256),
    ("128 × 128", 128, 128),
    ("64 × 64", 64, 64),
    ("32 × 32", 32, 32),
];

/// Alça do retângulo de seleção: x/y ∈ {-1, 0, 1} (esquerda/centro/direita).
#[derive(Clone, Copy, PartialEq)]
struct Alca {
    x: i8,
    y: i8,
}

enum Arrasto {
    Nova { ancora: Pos2 },
    Mover { desloc: Vec2 },
    Alca { alca: Alca, inicial: Rect },
}

struct Tarefa {
    rx: mpsc::Receiver<Result<DynamicImage, String>>,
    descricao: String,
    inicio: Instant,
}

struct App {
    doc: Option<Doc>,
    textura: Option<TextureHandle>,
    textura_suja: bool,

    zoom: f32,
    pan: Vec2,
    ajustar_pendente: bool,

    ferramenta: Ferramenta,
    sel: Option<Rect>,
    arrasto: Option<Arrasto>,
    proporcao: Proporcao,
    personalizada: (f32, f32),

    largura: u32,
    altura: u32,
    manter_proporcao: bool,
    escala: f64,
    opcoes: Opcoes,

    qualidade_jpeg: u8,
    status: String,
    tarefa: Option<Tarefa>,
    confirmar_fechar: bool,
    pode_fechar: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, arquivo: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx
            .all_styles_mut(|s| s.interaction.selectable_labels = false);
        let mut app = App {
            doc: None,
            textura: None,
            textura_suja: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            ajustar_pendente: true,
            ferramenta: Ferramenta::Recortar,
            sel: None,
            arrasto: None,
            proporcao: Proporcao::Livre,
            personalizada: (1.0, 1.0),
            largura: 0,
            altura: 0,
            manter_proporcao: false,
            escala: 2.0,
            opcoes: Opcoes::default(),
            qualidade_jpeg: 95,
            status: "Abra uma imagem: Ctrl+O, arraste um arquivo para a janela ou cole com Ctrl+V."
                .into(),
            tarefa: None,
            confirmar_fechar: false,
            pode_fechar: false,
        };
        if let Some(p) = arquivo {
            app.abrir(&p);
        }
        app
    }

    // ---- documento ----------------------------------------------------------

    fn abrir(&mut self, caminho: &Path) {
        match ops::abrir(caminho) {
            Ok(img) => {
                let (w, h) = (img.width(), img.height());
                self.novo_doc(img, Some(caminho.to_path_buf()));
                self.status = format!("Aberta: {} ({w} × {h})", caminho.display());
            }
            Err(e) => self.status = format!("Erro ao abrir {}: {e}", caminho.display()),
        }
    }

    fn novo_doc(&mut self, img: DynamicImage, caminho: Option<PathBuf>) {
        // operação em andamento era da imagem anterior: descarta o resultado
        // (a thread termina sozinha; o envio falha porque o receptor sumiu)
        if let Some(t) = self.tarefa.take() {
            self.status = format!("{} cancelado.", t.descricao);
        }
        self.doc = Some(Doc {
            img: Arc::new(img),
            caminho,
            desfazer: Vec::new(),
            refazer: Vec::new(),
            modificado: false,
        });
        self.imagem_mudou();
        // sugestão de método conforme o tamanho
        if let Some((w, h)) = self.doc.as_ref().map(Doc::tam) {
            self.escala = if w.max(h) <= 128 { 4.0 } else { 2.0 };
        }
    }

    fn imagem_mudou(&mut self) {
        self.textura_suja = true;
        self.ajustar_pendente = true;
        self.sel = None;
        self.arrasto = None;
        if let Some((w, h)) = self.doc.as_ref().map(Doc::tam) {
            self.largura = w;
            self.altura = h;
        }
    }

    fn aplicar(&mut self, nova: DynamicImage, descricao: String) {
        let Some(doc) = &mut self.doc else { return };
        let (w0, h0) = doc.tam();
        let antiga = std::mem::replace(&mut doc.img, Arc::new(nova));
        doc.desfazer.push((antiga, descricao.clone()));
        doc.refazer.clear();
        doc.modificado = true;
        // limita a memória do histórico, mas sempre deixa ao menos 1 passo
        let bytes = |v: &Vec<(Arc<DynamicImage>, String)>| {
            v.iter().map(|(i, _)| i.as_bytes().len()).sum::<usize>()
        };
        while doc.desfazer.len() > 1 && bytes(&doc.desfazer) > LIMITE_DESFAZER_BYTES {
            doc.desfazer.remove(0);
        }
        let (w, h) = doc.tam();
        self.status = format!("{descricao}: {w0} × {h0} ➡ {w} × {h}");
        self.imagem_mudou();
    }

    fn desfazer(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        let Some((img, desc)) = doc.desfazer.pop() else {
            return;
        };
        let atual = std::mem::replace(&mut doc.img, img);
        doc.refazer.push((atual, desc.clone()));
        doc.modificado = true;
        self.status = format!("Desfeito: {desc}");
        self.imagem_mudou();
    }

    fn refazer(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        let Some((img, desc)) = doc.refazer.pop() else {
            return;
        };
        let atual = std::mem::replace(&mut doc.img, img);
        doc.desfazer.push((atual, desc.clone()));
        doc.modificado = true;
        self.status = format!("Refeito: {desc}");
        self.imagem_mudou();
    }

    /// Roda uma operação pesada numa thread, sem travar a janela.
    fn em_segundo_plano(
        &mut self,
        ctx: &egui::Context,
        descricao: String,
        f: impl FnOnce(&DynamicImage) -> Result<DynamicImage, String> + Send + 'static,
    ) {
        let Some(doc) = &self.doc else { return };
        if self.tarefa.is_some() {
            return;
        }
        let img = doc.img.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f(&img));
            ctx.request_repaint();
        });
        self.status = format!("{descricao}…");
        self.tarefa = Some(Tarefa {
            rx,
            descricao,
            inicio: Instant::now(),
        });
    }

    fn checar_tarefa(&mut self) {
        let Some(t) = &self.tarefa else { return };
        let resultado = match t.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("a operação travou (falta de memória?)".into())
            }
        };
        let t = self.tarefa.take().expect("tarefa existe");
        match resultado {
            Ok(img) => {
                let seg = t.inicio.elapsed().as_secs_f32();
                self.aplicar(img, t.descricao);
                self.status.push_str(&format!("  ({seg:.2} s)"));
            }
            Err(e) => self.status = format!("Erro em {}: {e}", t.descricao),
        }
    }

    fn salvar(&mut self, como: bool) {
        let Some(doc) = &self.doc else { return };
        let destino = match (&doc.caminho, como) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let nome = doc
                    .caminho
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "imagem.png".into());
                let mut dlg = rfd::FileDialog::new()
                    .set_title("Salvar imagem")
                    .set_file_name(nome)
                    .add_filter("PNG (sem perdas)", &["png"])
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("WebP (sem perdas)", &["webp"])
                    .add_filter("BMP", &["bmp"])
                    .add_filter("TIFF", &["tif", "tiff"])
                    .add_filter("GIF", &["gif"])
                    .add_filter("ICO (até 256 px)", &["ico"])
                    .add_filter("TGA", &["tga"])
                    .add_filter("QOI", &["qoi"]);
                if let Some(dir) = doc.caminho.as_ref().and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(dir);
                }
                dlg.save_file().map(|mut p| {
                    if p.extension().is_none() {
                        p.set_extension("png");
                    }
                    p
                })
            }
        };
        let Some(destino) = destino else { return };
        let doc = self.doc.as_mut().expect("doc existe");
        match ops::salvar(&doc.img, &destino, self.qualidade_jpeg) {
            Ok(()) => {
                let tam = std::fs::metadata(&destino).map(|m| m.len()).unwrap_or(0);
                self.status = format!("Salva: {} ({})", destino.display(), tamanho_legivel(tam));
                doc.caminho = Some(destino);
                doc.modificado = false;
            }
            Err(e) => self.status = format!("Erro ao salvar: {e}"),
        }
    }

    fn dialogo_abrir(&mut self) {
        let mut dlg = rfd::FileDialog::new()
            .set_title("Abrir imagem")
            .add_filter("Imagens", ops::EXTENSOES_ABRIR)
            .add_filter("Todos os arquivos", &["*"]);
        if let Some(dir) = self
            .doc
            .as_ref()
            .and_then(|d| d.caminho.as_ref())
            .and_then(|p| p.parent())
        {
            dlg = dlg.set_directory(dir);
        }
        if let Some(p) = dlg.pick_file() {
            self.abrir(&p);
        }
    }

    fn copiar(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.doc else { return };
        let rgba = doc.img.to_rgba8();
        let ci = ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            &rgba,
        );
        ctx.copy_image(ci);
        self.status = "Imagem copiada para a área de transferência.".into();
    }

    fn colar_imagem(&mut self, ci: &ColorImage) {
        let [w, h] = ci.size;
        let bytes: Vec<u8> = ci
            .pixels
            .iter()
            .flat_map(|c| c.to_srgba_unmultiplied())
            .collect();
        if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
            self.novo_doc(DynamicImage::ImageRgba8(img), None);
            self.status = format!("Imagem colada ({w} × {h}). Use \"Salvar como\" para gravar.");
        }
    }

    // ---- recorte --------------------------------------------------------------

    fn razao(&self) -> Option<f32> {
        match self.proporcao {
            Proporcao::Livre => None,
            Proporcao::Original => self
                .doc
                .as_ref()
                .map(|d| d.img.width() as f32 / d.img.height() as f32),
            Proporcao::Fixa(a, b) => Some(a as f32 / b as f32),
            Proporcao::Personalizada => {
                let (a, b) = self.personalizada;
                (a > 0.0 && b > 0.0).then(|| a / b)
            }
        }
    }

    /// Seleção em pixels inteiros, já com a proporção exata aplicada.
    fn sel_final(&self) -> Option<(u32, u32, u32, u32)> {
        let (iw, ih) = self.doc.as_ref()?.tam();
        let s = self.sel?;
        let x = (s.min.x.round().max(0.0) as u32).min(iw - 1);
        let y = (s.min.y.round().max(0.0) as u32).min(ih - 1);
        let mut w = (s.width().round() as u32).clamp(1, iw - x);
        let mut h = (s.height().round() as u32).clamp(1, ih - y);
        if let Proporcao::Fixa(a, b) = self.proporcao {
            // proporção exata: w = k·a, h = k·b (com a:b reduzido)
            let g = mdc(a, b);
            let (a, b) = (a / g, b / g);
            let kmax = ((iw - x) / a).min((ih - y) / b);
            if kmax >= 1 {
                let k = ((w as f32 / a as f32).round() as u32).clamp(1, kmax);
                w = k * a;
                h = k * b;
            }
        } else if let Some(r) = self.razao() {
            h = ((w as f32 / r).round() as u32).clamp(1, ih - y);
            if (w as f32 / h as f32 - r).abs() > 0.01 {
                w = ((h as f32 * r).round() as u32).clamp(1, iw - x);
            }
        }
        Some((x, y, w, h))
    }

    /// Maior retângulo centrado com a proporção atual, dentro de `dentro`.
    fn encaixar_proporcao(&mut self) {
        let Some((iw, ih)) = self.doc.as_ref().map(Doc::tam) else {
            return;
        };
        let Some(r) = self.razao() else { return };
        let base = self
            .sel
            .unwrap_or(Rect::from_min_size(Pos2::ZERO, vec2(iw as f32, ih as f32)));
        let base = if base.width() < 2.0 || base.height() < 2.0 {
            Rect::from_min_size(Pos2::ZERO, vec2(iw as f32, ih as f32))
        } else {
            base
        };
        let (mut w, mut h) = (base.width(), base.width() / r);
        if h > base.height() {
            h = base.height();
            w = h * r;
        }
        self.sel = Some(Rect::from_center_size(base.center(), vec2(w, h)));
    }

    fn aplicar_recorte(&mut self) {
        let Some((x, y, w, h)) = self.sel_final() else {
            return;
        };
        let Some(doc) = &self.doc else { return };
        if (w, h) == doc.tam() {
            self.status = "A seleção é a imagem inteira; nada para recortar.".into();
            return;
        }
        let nova = doc.img.crop_imm(x, y, w, h);
        self.aplicar(nova, "Recorte".into());
    }

    fn atualizar_arrasto(&mut self, p: Pos2) {
        let Some((iw, ih)) = self.doc.as_ref().map(Doc::tam) else {
            return;
        };
        let limites = vec2(iw as f32, ih as f32);
        let razao = self.razao();
        let p = pos2(p.x.clamp(0.0, limites.x), p.y.clamp(0.0, limites.y));
        let nova = match self.arrasto {
            Some(Arrasto::Nova { ancora }) => ret_ancora(ancora, p, razao, limites),
            Some(Arrasto::Mover { desloc }) => {
                let Some(s) = self.sel else { return };
                let min = p - desloc;
                let min = pos2(
                    min.x.clamp(0.0, (limites.x - s.width()).max(0.0)),
                    min.y.clamp(0.0, (limites.y - s.height()).max(0.0)),
                );
                Rect::from_min_size(min, s.size())
            }
            Some(Arrasto::Alca { alca, inicial }) => {
                arrastar_alca(alca, inicial, p, razao, limites)
            }
            None => return,
        };
        self.sel = Some(nova);
    }

    // ---- interface ------------------------------------------------------------

    fn atalhos(&mut self, ctx: &egui::Context) {
        let atalho = |m, k| KeyboardShortcut::new(m, k);
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let (abrir, salvar_como, salvar, refazer, refazer2, desfazer, ajustar, cem) = ctx
            .input_mut(|i| {
                (
                    i.consume_shortcut(&atalho(cmd, Key::O)),
                    i.consume_shortcut(&atalho(cmd_shift, Key::S)),
                    i.consume_shortcut(&atalho(cmd, Key::S)),
                    i.consume_shortcut(&atalho(cmd_shift, Key::Z)),
                    i.consume_shortcut(&atalho(cmd, Key::Y)),
                    i.consume_shortcut(&atalho(cmd, Key::Z)),
                    i.consume_shortcut(&atalho(cmd, Key::Num0)),
                    i.consume_shortcut(&atalho(cmd, Key::Num1)),
                )
            });
        if abrir {
            self.dialogo_abrir();
        }
        if salvar_como {
            self.salvar(true);
        } else if salvar {
            self.salvar(false);
        }
        if self.tarefa.is_none() && !ctx.egui_wants_keyboard_input() {
            if refazer || refazer2 {
                self.refazer();
            } else if desfazer {
                self.desfazer();
            }
            if self.ferramenta == Ferramenta::Recortar && self.sel.is_some() {
                if ctx.input(|i| i.key_pressed(Key::Enter)) {
                    self.aplicar_recorte();
                }
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    self.sel = None;
                }
            }
        }
        if ajustar {
            self.ajustar_pendente = true;
        }
        if cem {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
        }

        // copiar / colar (o egui-winit já transforma Ctrl+C/V em eventos)
        let eventos = ctx.input(|i| i.events.clone());
        if !ctx.egui_wants_keyboard_input() {
            for ev in eventos {
                match ev {
                    egui::Event::Copy => self.copiar(ctx),
                    egui::Event::PasteImage(ci) => self.colar_imagem(&ci),
                    egui::Event::Paste(texto) => {
                        // texto colado que é um caminho de arquivo (ex.: copiado do Thunar)
                        // ou "Copiar como caminho" do Explorer, que vem entre aspas
                        let linha = texto.lines().next().unwrap_or("").trim().trim_matches('"');
                        let caminho = linha
                            .strip_prefix("file://")
                            .map(decodificar_url)
                            .unwrap_or(linha.to_string());
                        // file:///C:/foto.png ➡ C:/foto.png
                        #[cfg(windows)]
                        let caminho = match caminho.strip_prefix('/') {
                            Some(r) if r.as_bytes().get(1) == Some(&b':') => r.to_string(),
                            _ => caminho,
                        };
                        let p = PathBuf::from(caminho);
                        if p.is_file() {
                            self.abrir(&p);
                        }
                    }
                    _ => {}
                }
            }
        }

        let soltos: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if let Some(p) = soltos.first() {
            self.abrir(p);
        }
    }

    fn painel(&mut self, ui: &mut egui::Ui) {
        let ocupado = self.tarefa.is_some();
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("📂 Abrir").on_hover_text("Ctrl+O").clicked() {
                self.dialogo_abrir();
            }
            let tem = self.doc.is_some();
            if ui
                .add_enabled(tem, egui::Button::new("💾 Salvar"))
                .on_hover_text("Ctrl+S")
                .clicked()
            {
                self.salvar(false);
            }
            if ui
                .add_enabled(tem, egui::Button::new("Salvar como…"))
                .on_hover_text("Ctrl+Shift+S")
                .clicked()
            {
                self.salvar(true);
            }
        });
        ui.horizontal(|ui| {
            let (pode_desfazer, pode_refazer) = self
                .doc
                .as_ref()
                .map(|d| (!d.desfazer.is_empty(), !d.refazer.is_empty()))
                .unwrap_or_default();
            let dica_desfazer = self
                .doc
                .as_ref()
                .and_then(|d| d.desfazer.last())
                .map(|(_, s)| s.clone());
            let r = ui.add_enabled(pode_desfazer && !ocupado, egui::Button::new("⟲ Desfazer"));
            if r.on_hover_text(format!("Ctrl+Z  {}", dica_desfazer.unwrap_or_default()))
                .clicked()
            {
                self.desfazer();
            }
            if ui
                .add_enabled(pode_refazer && !ocupado, egui::Button::new("⟳ Refazer"))
                .on_hover_text("Ctrl+Shift+Z")
                .clicked()
            {
                self.refazer();
            }
        });

        let Some(doc) = &self.doc else {
            ui.separator();
            ui.label("Nenhuma imagem aberta.");
            ui.add_space(4.0);
            ui.weak(
                "Formatos: PNG, JPEG, WebP, GIF, BMP, TIFF, TGA, ICO, QOI, PNM, DDS, EXR, HDR.",
            );
            return;
        };
        let (iw, ih) = doc.tam();
        ui.separator();
        ui.label(
            egui::RichText::new(format!("{iw} × {ih} px"))
                .strong()
                .size(16.0),
        );
        ui.weak(format!(
            "proporção {} · {:.2} MP · {}",
            proporcao_txt(iw, ih),
            (iw as f64 * ih as f64) / 1e6,
            tipo_txt(doc.img.color())
        ));
        ui.separator();

        ui.horizontal(|ui| {
            let mut f = self.ferramenta;
            ui.selectable_value(&mut f, Ferramenta::Recortar, "✂ Recortar");
            ui.selectable_value(&mut f, Ferramenta::Redimensionar, "⬌ Redimensionar");
            ui.selectable_value(&mut f, Ferramenta::Escala, "🔍 Escala");
            if f != self.ferramenta {
                self.ferramenta = f;
                if let Some((w, h)) = self.doc.as_ref().map(Doc::tam) {
                    self.largura = w;
                    self.altura = h;
                }
            }
        });
        ui.add_space(6.0);

        ui.add_enabled_ui(!ocupado, |ui| match self.ferramenta {
            Ferramenta::Recortar => self.painel_recorte(ui, iw, ih),
            Ferramenta::Redimensionar => self.painel_redimensionar(ui, iw, ih),
            Ferramenta::Escala => self.painel_escala(ui, iw, ih),
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_enabled_ui(!ocupado, |ui| {
            ui.label("Girar / espelhar (sem perdas)");
            ui.horizontal_wrapped(|ui| {
                let img = self.doc.as_ref().map(|d| d.img.clone());
                let Some(img) = img else { return };
                if ui.button("⟲ 90°").clicked() {
                    self.aplicar(img.rotate270(), "Girar 90° anti-horário".into());
                } else if ui.button("⟳ 90°").clicked() {
                    self.aplicar(img.rotate90(), "Girar 90° horário".into());
                } else if ui.button("180°").clicked() {
                    self.aplicar(img.rotate180(), "Girar 180°".into());
                } else if ui
                    .button("↔ Espelhar")
                    .on_hover_text("Horizontal")
                    .clicked()
                {
                    self.aplicar(img.fliph(), "Espelhar horizontal".into());
                } else if ui.button("↕ Inverter").on_hover_text("Vertical").clicked() {
                    self.aplicar(img.flipv(), "Espelhar vertical".into());
                }
            });
        });

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Qualidade JPEG");
            ui.add(egui::Slider::new(&mut self.qualidade_jpeg, 50..=100));
        });
        ui.weak("PNG e WebP salvam sem perda nenhuma. JPEG sempre recomprime.");
    }

    fn painel_recorte(&mut self, ui: &mut egui::Ui, iw: u32, ih: u32) {
        ui.label("Arraste na imagem para selecionar. Puxe as alças para ajustar, arraste por dentro para mover.");
        ui.add_space(4.0);
        let anterior = self.proporcao;
        ui.horizontal(|ui| {
            ui.label("Proporção");
            let atual = PROPORCOES
                .iter()
                .find(|(_, p)| *p == self.proporcao)
                .map(|(n, _)| n.to_string())
                .unwrap_or_else(|| match self.proporcao {
                    Proporcao::Fixa(a, b) => format!("{a}:{b}"),
                    _ => "?".into(),
                });
            egui::ComboBox::from_id_salt("proporcao")
                .selected_text(atual)
                .width(170.0)
                .height(500.0)
                .show_ui(ui, |ui| {
                    for (nome, p) in PROPORCOES {
                        ui.selectable_value(&mut self.proporcao, *p, *nome);
                    }
                });
        });
        if self.proporcao == Proporcao::Personalizada {
            ui.horizontal(|ui| {
                ui.label("   ");
                let a = ui.add(
                    egui::DragValue::new(&mut self.personalizada.0)
                        .range(0.01..=10000.0)
                        .speed(0.1),
                );
                ui.label(":");
                let b = ui.add(
                    egui::DragValue::new(&mut self.personalizada.1)
                        .range(0.01..=10000.0)
                        .speed(0.1),
                );
                if a.changed() || b.changed() {
                    self.encaixar_proporcao();
                }
            });
        }
        ui.horizontal(|ui| {
            let gira = !matches!(self.proporcao, Proporcao::Livre | Proporcao::Original);
            if ui
                .add_enabled(gira, egui::Button::new("↻ Girar proporção"))
                .on_hover_text("Troca deitado ↔ em pé (ex.: 16:9 ➡ 9:16)")
                .clicked()
            {
                self.proporcao = match self.proporcao {
                    Proporcao::Fixa(a, b) => Proporcao::Fixa(b, a),
                    p => {
                        self.personalizada = (self.personalizada.1, self.personalizada.0);
                        p
                    }
                };
                self.encaixar_proporcao();
            }
            if ui.button("Selecionar tudo").clicked() {
                self.sel = Some(Rect::from_min_size(Pos2::ZERO, vec2(iw as f32, ih as f32)));
                self.encaixar_proporcao();
            }
        });
        if self.proporcao != anterior {
            self.encaixar_proporcao();
        }

        ui.add_space(6.0);
        if let Some((x, y, w, h)) = self.sel_final() {
            let (mut nx, mut ny, mut nw, mut nh) = (x, y, w, h);
            egui::Grid::new("sel")
                .num_columns(4)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    ui.label("X");
                    ui.add(egui::DragValue::new(&mut nx).range(0..=iw - 1));
                    ui.label("Largura");
                    ui.add(egui::DragValue::new(&mut nw).range(1..=iw));
                    ui.end_row();
                    ui.label("Y");
                    ui.add(egui::DragValue::new(&mut ny).range(0..=ih - 1));
                    ui.label("Altura");
                    ui.add(egui::DragValue::new(&mut nh).range(1..=ih));
                    ui.end_row();
                });
            if (nx, ny, nw, nh) != (x, y, w, h) {
                let r = self.razao();
                let (mut fw, mut fh) = (nw as f32, nh as f32);
                if let Some(r) = r {
                    if nw != w {
                        fh = fw / r;
                    } else if nh != h {
                        fw = fh * r;
                    }
                }
                let fx = (nx as f32).min(iw as f32 - 1.0);
                let fy = (ny as f32).min(ih as f32 - 1.0);
                fw = fw.min(iw as f32 - fx);
                fh = fh.min(ih as f32 - fy);
                if let Some(r) = r {
                    // se bateu na borda, encolhe mantendo a proporção
                    if fw / fh > r + 1e-3 {
                        fw = fh * r;
                    } else if fw / fh < r - 1e-3 {
                        fh = fw / r;
                    }
                }
                self.sel = Some(Rect::from_min_size(pos2(fx, fy), vec2(fw, fh)));
            }
            ui.weak(format!("Resultado: {w} × {h} px ({})", proporcao_txt(w, h)));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button("✔ Aplicar recorte")
                    .on_hover_text("Enter ou duplo clique na seleção")
                    .clicked()
                {
                    self.aplicar_recorte();
                }
                if ui.button("Cancelar").on_hover_text("Esc").clicked() {
                    self.sel = None;
                }
            });
        } else {
            ui.weak("Nenhuma seleção.");
        }
    }

    fn painel_redimensionar(&mut self, ui: &mut egui::Ui, iw: u32, ih: u32) {
        ui.label(
            "Define largura e altura livres. Se a proporção mudar, a imagem estica ou achata.",
        );
        ui.add_space(4.0);
        let (w0, h0) = (self.largura, self.altura);
        egui::Grid::new("redim")
            .num_columns(3)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.label("Largura");
                ui.add(
                    egui::DragValue::new(&mut self.largura)
                        .range(1..=65535)
                        .suffix(" px"),
                );
                ui.end_row();
                ui.label("Altura");
                ui.add(
                    egui::DragValue::new(&mut self.altura)
                        .range(1..=65535)
                        .suffix(" px"),
                );
                ui.end_row();
            });
        if self.manter_proporcao {
            let r = iw as f64 / ih as f64;
            if self.largura != w0 {
                self.altura = ((self.largura as f64 / r).round() as u32).max(1);
            } else if self.altura != h0 {
                self.largura = ((self.altura as f64 * r).round() as u32).max(1);
            }
        }
        ui.checkbox(&mut self.manter_proporcao, "Manter proporção");
        ui.horizontal(|ui| {
            ui.label("Tamanhos comuns");
            egui::ComboBox::from_id_salt("tamanhos")
                .selected_text("escolher…")
                .height(500.0)
                .show_ui(ui, |ui| {
                    for (nome, w, h) in TAMANHOS_COMUNS {
                        if ui.selectable_label(false, *nome).clicked() {
                            self.largura = *w;
                            self.altura = *h;
                            self.manter_proporcao = false;
                        }
                    }
                });
            if ui.button("Original").clicked() {
                self.largura = iw;
                self.altura = ih;
            }
        });
        ui.add_space(4.0);
        self.ui_metodo(ui, iw, ih, self.largura, self.altura);

        let (w, h) = (self.largura, self.altura);
        let r_antes = iw as f64 / ih as f64;
        let r_depois = w as f64 / h as f64;
        ui.add_space(4.0);
        ui.label(format!("Resultado: {w} × {h} px ({})", proporcao_txt(w, h)));
        if (r_antes / r_depois - 1.0).abs() > 0.005 {
            let como = if r_depois > r_antes {
                "esticada na horizontal (achatada)"
            } else {
                "esticada na vertical (afinada)"
            };
            ui.colored_label(
                Color32::from_rgb(240, 190, 90),
                format!(
                    "⚠ Proporção muda de {} para {}: imagem {como}.",
                    proporcao_txt(iw, ih),
                    proporcao_txt(w, h)
                ),
            );
        }
        self.botao_reamostrar(ui, iw, ih, w, h, "Redimensionar");
    }

    fn painel_escala(&mut self, ui: &mut egui::Ui, iw: u32, ih: u32) {
        ui.label("Muda a resolução mantendo a proporção. Ex.: 20×20 ➡ 500×500 (2500%).");
        ui.add_space(4.0);
        let mut pct = self.escala * 100.0;
        ui.horizontal(|ui| {
            ui.label("Escala");
            if ui
                .add(
                    egui::DragValue::new(&mut pct)
                        .range(0.1..=10000.0)
                        .speed(1.0)
                        .suffix(" %")
                        .max_decimals(2),
                )
                .changed()
            {
                self.escala = pct / 100.0;
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (rot, f) in [
                ("25%", 0.25),
                ("50%", 0.5),
                ("2x", 2.0),
                ("3x", 3.0),
                ("4x", 4.0),
                ("8x", 8.0),
                ("10x", 10.0),
            ] {
                if ui
                    .selectable_label((self.escala - f).abs() < 1e-9, rot)
                    .clicked()
                {
                    self.escala = f;
                }
            }
        });
        let mut w = ((iw as f64 * self.escala).round() as u32).max(1);
        let mut h = ((ih as f64 * self.escala).round() as u32).max(1);
        egui::Grid::new("escala")
            .num_columns(2)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.label("ou largura");
                if ui
                    .add(egui::DragValue::new(&mut w).range(1..=65535).suffix(" px"))
                    .changed()
                {
                    self.escala = w as f64 / iw as f64;
                }
                ui.end_row();
                ui.label("ou altura");
                if ui
                    .add(egui::DragValue::new(&mut h).range(1..=65535).suffix(" px"))
                    .changed()
                {
                    self.escala = h as f64 / ih as f64;
                }
                ui.end_row();
            });
        let w = ((iw as f64 * self.escala).round() as u32).max(1);
        let h = ((ih as f64 * self.escala).round() as u32).max(1);
        ui.add_space(4.0);
        self.ui_metodo(ui, iw, ih, w, h);
        ui.add_space(4.0);
        ui.label(format!("Resultado: {w} × {h} px"));
        self.botao_reamostrar(ui, iw, ih, w, h, "Escala");
    }

    fn ui_metodo(&mut self, ui: &mut egui::Ui, iw: u32, ih: u32, w: u32, h: u32) {
        ui.horizontal(|ui| {
            ui.label("Método");
            egui::ComboBox::from_id_salt("metodo")
                .selected_text(self.opcoes.metodo.nome())
                .width(210.0)
                .show_ui(ui, |ui| {
                    for m in Metodo::TODOS {
                        ui.selectable_value(&mut self.opcoes.metodo, m, m.nome())
                            .on_hover_text(m.dica());
                    }
                });
        });
        ui.weak(self.opcoes.metodo.dica());
        let ampliando = w > iw || h > ih;
        if ampliando
            && iw.max(ih) <= 256
            && !matches!(self.opcoes.metodo, Metodo::Vizinho | Metodo::PixelArt)
        {
            ui.weak("Dica: se for pixel art ou ícone, \"Vizinho\" ou \"Pixel art\" ficam nítidos.");
        }
        let suave = !matches!(self.opcoes.metodo, Metodo::Vizinho | Metodo::PixelArt);
        ui.add_enabled_ui(suave, |ui| {
            ui.horizontal(|ui| {
                ui.label("Nitidez");
                ui.add(egui::Slider::new(&mut self.opcoes.nitidez, 0.0..=2.0).step_by(0.05))
                    .on_hover_text("Realça bordas depois de reamostrar. Ajuda ao ampliar; exagero cria halos.");
            });
            ui.checkbox(&mut self.opcoes.linear, "Luz linear (cores mais corretas, mais lento)").on_hover_text(
                "Faz a média das cores em luz linear em vez de sRGB. Evita escurecer detalhes finos \
                 e bordas coloridas, principalmente ao reduzir.",
            );
        });
    }

    fn botao_reamostrar(
        &mut self,
        ui: &mut egui::Ui,
        iw: u32,
        ih: u32,
        w: u32,
        h: u32,
        nome: &str,
    ) {
        let mp = w as f64 * h as f64 / 1e6;
        let grande = mp > LIMITE_MEGAPIXELS;
        if grande {
            ui.colored_label(
                Color32::from_rgb(240, 110, 90),
                format!("Grande demais ({mp:.0} MP; limite {LIMITE_MEGAPIXELS:.0} MP)."),
            );
        }
        ui.add_space(4.0);
        let igual = (w, h) == (iw, ih);
        let botao = ui.add_enabled(
            !igual && !grande,
            egui::Button::new(format!("✔ Aplicar ({w} × {h})")).min_size(vec2(200.0, 28.0)),
        );
        if botao.clicked() {
            let op = self.opcoes;
            let desc = format!(
                "{nome} ({})",
                op.metodo.nome().split(" (").next().unwrap_or("")
            );
            let ctx = ui.ctx().clone();
            self.em_segundo_plano(&ctx, desc, move |img| ops::redimensionar(img, w, h, op));
        }
    }

    fn atualizar_textura(&mut self, ctx: &egui::Context) {
        if !self.textura_suja {
            return;
        }
        self.textura_suja = false;
        let Some(doc) = &self.doc else {
            self.textura = None;
            return;
        };
        let max = ctx.input(|i| i.max_texture_side) as u32;
        let (w, h) = doc.tam();
        // imagem maior que a GPU aceita: mostra uma prévia reduzida (o arquivo continua inteiro)
        let previa = if w.max(h) > max {
            let f = max as f64 / w.max(h) as f64;
            let (pw, ph) = (
                ((w as f64 * f) as u32).max(1),
                ((h as f64 * f) as u32).max(1),
            );
            let op = Opcoes {
                metodo: Metodo::Bilinear,
                ..Default::default()
            };
            ops::redimensionar(&doc.img, pw, ph, op)
                .unwrap_or_else(|_| doc.img.thumbnail(pw, ph))
                .to_rgba8()
        } else {
            doc.img.to_rgba8()
        };
        let ci = ColorImage::from_rgba_unmultiplied(
            [previa.width() as usize, previa.height() as usize],
            &previa,
        );
        // ampliado: pixels nítidos (vê o pixel real); reduzido: mipmap suave
        let opcoes = TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::default()
        };
        match &mut self.textura {
            Some(t) => t.set(ci, opcoes),
            None => self.textura = Some(ctx.load_texture("imagem", ci, opcoes)),
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let area = resp.rect;
        painter.rect_filled(area, CornerRadius::ZERO, Color32::from_gray(22));

        let arquivos_sobre = ui.input(|i| !i.raw.hovered_files.is_empty());
        let Some((iw, ih)) = self.doc.as_ref().map(Doc::tam) else {
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "Abra uma imagem (Ctrl+O), arraste um arquivo para cá\nou cole uma imagem (Ctrl+V)",
                egui::FontId::proportional(18.0),
                Color32::from_gray(140),
            );
            if arquivos_sobre {
                painter.rect_stroke(
                    area.shrink(8.0),
                    8.0,
                    Stroke::new(3.0, Color32::from_rgb(90, 160, 255)),
                    StrokeKind::Inside,
                );
            }
            return;
        };
        let tam = vec2(iw as f32, ih as f32);

        if self.ajustar_pendente {
            self.ajustar_pendente = false;
            let z = ((area.width() - 40.0) / tam.x).min((area.height() - 40.0) / tam.y);
            self.zoom = z.clamp(0.01, 64.0);
            self.pan = Vec2::ZERO;
        }

        // zoom na roda do mouse, centrado no cursor
        if let Some(cursor) = resp.hover_pos() {
            let (rolagem, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let fator = zoom_delta * (rolagem * 0.003).exp();
            if (fator - 1.0).abs() > 1e-4 {
                let novo = (self.zoom * fator).clamp(0.01, 128.0);
                let origem = area.center() + self.pan - tam * self.zoom / 2.0;
                let p_img = (cursor - origem) / self.zoom;
                let nova_origem = cursor - p_img * novo;
                self.pan = nova_origem - (area.center() - tam * novo / 2.0);
                self.zoom = novo;
            }
        }

        // pan: botão do meio, direito, espaço+arrastar, ou esquerdo fora do recorte
        let espaco = ui.input(|i| i.key_down(Key::Space));
        let recortando = self.ferramenta == Ferramenta::Recortar && !espaco;
        if resp.dragged_by(egui::PointerButton::Middle)
            || resp.dragged_by(egui::PointerButton::Secondary)
            || (resp.dragged_by(egui::PointerButton::Primary) && !recortando)
        {
            self.pan += resp.drag_delta();
        }

        let origem = area.center() + self.pan - tam * self.zoom / 2.0;
        let zoom = self.zoom;
        let para_tela = |p: Pos2| origem + p.to_vec2() * zoom;
        let para_img = |s: Pos2| ((s - origem) / zoom).to_pos2();
        let ret_img = Rect::from_min_size(origem, tam * zoom);

        // interação de recorte
        if recortando {
            if resp.drag_started_by(egui::PointerButton::Primary)
                && let Some(inicio) = ui.input(|i| i.pointer.press_origin())
            {
                let alca = self.sel.and_then(|s| {
                    achar_alca(
                        Rect::from_min_max(para_tela(s.min), para_tela(s.max)),
                        inicio,
                    )
                });
                let p = para_img(inicio);
                self.arrasto = Some(match (alca, self.sel) {
                    (Some(a), Some(s)) => Arrasto::Alca {
                        alca: a,
                        inicial: s,
                    },
                    (None, Some(s)) if s.contains(p) => Arrasto::Mover { desloc: p - s.min },
                    _ => Arrasto::Nova {
                        ancora: pos2(p.x.clamp(0.0, tam.x), p.y.clamp(0.0, tam.y)),
                    },
                });
            }
            if resp.dragged_by(egui::PointerButton::Primary)
                && let Some(p) = resp.interact_pointer_pos()
            {
                self.atualizar_arrasto(para_img(p));
            }
            if resp.drag_stopped() {
                self.arrasto = None;
                if self
                    .sel
                    .is_some_and(|s| s.width() < 1.0 || s.height() < 1.0)
                {
                    self.sel = None;
                }
            }
            if resp.double_clicked()
                && let (Some(s), Some(p)) = (self.sel, resp.interact_pointer_pos())
                && s.contains(para_img(p))
            {
                self.aplicar_recorte();
            } else if resp.clicked()
                && let (Some(s), Some(p)) = (self.sel, resp.interact_pointer_pos())
                && !s.contains(para_img(p))
            {
                self.sel = None;
            }

            // cursor conforme o que está embaixo
            if let Some(h) = resp.hover_pos() {
                let cursor = match self.sel {
                    Some(s) => {
                        match achar_alca(Rect::from_min_max(para_tela(s.min), para_tela(s.max)), h)
                        {
                            Some(Alca { x: 0, .. }) => CursorIcon::ResizeVertical,
                            Some(Alca { y: 0, .. }) => CursorIcon::ResizeHorizontal,
                            Some(Alca { x, y }) if x == y => CursorIcon::ResizeNwSe,
                            Some(_) => CursorIcon::ResizeNeSw,
                            None if s.contains(para_img(h)) => CursorIcon::Move,
                            None => CursorIcon::Crosshair,
                        }
                    }
                    None => CursorIcon::Crosshair,
                };
                ui.ctx().set_cursor_icon(cursor);
            }
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(if resp.dragged() {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            });
        }

        // fundo xadrez (transparência) só na parte visível
        let visivel = ret_img.intersect(area);
        if visivel.is_positive() {
            let q = 10.0;
            let (c1, c2) = (Color32::from_gray(200), Color32::from_gray(150));
            painter.rect_filled(visivel, CornerRadius::ZERO, c1);
            let x0 = ((visivel.min.x - ret_img.min.x) / q).floor() as i64;
            let y0 = ((visivel.min.y - ret_img.min.y) / q).floor() as i64;
            let x1 = ((visivel.max.x - ret_img.min.x) / q).ceil() as i64;
            let y1 = ((visivel.max.y - ret_img.min.y) / q).ceil() as i64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    if (xx + yy) % 2 != 0 {
                        let r = Rect::from_min_size(
                            ret_img.min + vec2(xx as f32 * q, yy as f32 * q),
                            vec2(q, q),
                        );
                        painter.rect_filled(r.intersect(visivel), CornerRadius::ZERO, c2);
                    }
                }
            }
        }
        if let Some(t) = &self.textura {
            painter.image(
                t.id(),
                ret_img,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        painter.rect_stroke(
            ret_img,
            CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_gray(70)),
            StrokeKind::Outside,
        );

        // grade de pixels quando bem ampliado
        if zoom >= 12.0 && visivel.is_positive() {
            let cor = Color32::from_black_alpha(50);
            let px0 = ((visivel.min.x - origem.x) / zoom).floor() as i64;
            let px1 = ((visivel.max.x - origem.x) / zoom).ceil() as i64;
            let py0 = ((visivel.min.y - origem.y) / zoom).floor() as i64;
            let py1 = ((visivel.max.y - origem.y) / zoom).ceil() as i64;
            for x in px0..=px1 {
                let sx = origem.x + x as f32 * zoom;
                painter.line_segment(
                    [pos2(sx, visivel.min.y), pos2(sx, visivel.max.y)],
                    Stroke::new(1.0, cor),
                );
            }
            for y in py0..=py1 {
                let sy = origem.y + y as f32 * zoom;
                painter.line_segment(
                    [pos2(visivel.min.x, sy), pos2(visivel.max.x, sy)],
                    Stroke::new(1.0, cor),
                );
            }
        }

        // seleção de recorte
        if self.ferramenta == Ferramenta::Recortar
            && let Some((x, y, w, h)) = self.sel_final()
        {
            let s = Rect::from_min_max(
                para_tela(pos2(x as f32, y as f32)),
                para_tela(pos2((x + w) as f32, (y + h) as f32)),
            );
            let escuro = Color32::from_black_alpha(150);
            let r = ret_img;
            for parte in [
                Rect::from_min_max(r.min, pos2(r.max.x, s.min.y)),
                Rect::from_min_max(pos2(r.min.x, s.max.y), r.max),
                Rect::from_min_max(pos2(r.min.x, s.min.y), pos2(s.min.x, s.max.y)),
                Rect::from_min_max(pos2(s.max.x, s.min.y), pos2(r.max.x, s.max.y)),
            ] {
                if parte.is_positive() {
                    painter.rect_filled(parte, CornerRadius::ZERO, escuro);
                }
            }
            // regra dos terços
            let guia = Stroke::new(1.0, Color32::from_white_alpha(70));
            for k in [1.0 / 3.0, 2.0 / 3.0] {
                let gx = s.min.x + s.width() * k;
                let gy = s.min.y + s.height() * k;
                painter.line_segment([pos2(gx, s.min.y), pos2(gx, s.max.y)], guia);
                painter.line_segment([pos2(s.min.x, gy), pos2(s.max.x, gy)], guia);
            }
            painter.rect_stroke(
                s,
                CornerRadius::ZERO,
                Stroke::new(3.0, Color32::from_black_alpha(160)),
                StrokeKind::Middle,
            );
            painter.rect_stroke(
                s,
                CornerRadius::ZERO,
                Stroke::new(1.0, Color32::WHITE),
                StrokeKind::Middle,
            );
            for p in pontos_alcas(s) {
                let q = Rect::from_center_size(p, vec2(9.0, 9.0));
                painter.rect_filled(q, CornerRadius::ZERO, Color32::WHITE);
                painter.rect_stroke(
                    q,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, Color32::BLACK),
                    StrokeKind::Inside,
                );
            }
            let rotulo = format!("{w} × {h}");
            let pos = pos2(s.min.x, (s.min.y - 6.0).max(area.min.y + 16.0));
            let galeria =
                painter.layout_no_wrap(rotulo, egui::FontId::proportional(13.0), Color32::WHITE);
            let fundo =
                Rect::from_min_size(pos - vec2(0.0, galeria.size().y), galeria.size()).expand(3.0);
            painter.rect_filled(fundo, 3.0, Color32::from_black_alpha(190));
            painter.galley(fundo.min + vec2(3.0, 3.0), galeria, Color32::WHITE);
        }

        if arquivos_sobre {
            painter.rect_stroke(
                area.shrink(8.0),
                8.0,
                Stroke::new(3.0, Color32::from_rgb(90, 160, 255)),
                StrokeKind::Inside,
            );
        }

        // coordenada sob o cursor (para a barra de status)
        let sob = resp
            .hover_pos()
            .map(para_img)
            .filter(|p| p.x >= 0.0 && p.y >= 0.0 && p.x < tam.x && p.y < tam.y);
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::unique("cursor_px"), sob));
    }

    fn barra_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(t) = &self.tarefa {
                ui.spinner();
                ui.label(format!(
                    "{}… {:.1} s",
                    t.descricao,
                    t.inicio.elapsed().as_secs_f32()
                ));
            } else {
                ui.label(&self.status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.doc.is_none() {
                    return;
                }
                if ui.small_button("100%").on_hover_text("Ctrl+1").clicked() {
                    self.zoom = 1.0;
                    self.pan = Vec2::ZERO;
                }
                if ui.small_button("Ajustar").on_hover_text("Ctrl+0").clicked() {
                    self.ajustar_pendente = true;
                }
                ui.label(format!("zoom {:.0}%", self.zoom * 100.0));
                let sob: Option<Pos2> = ui
                    .ctx()
                    .data(|d| d.get_temp(egui::Id::unique("cursor_px")))
                    .flatten();
                if let Some(p) = sob {
                    ui.weak(format!("x {} · y {}", p.x as u32, p.y as u32));
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.checar_tarefa();
        self.atalhos(&ctx);
        self.atualizar_textura(&ctx);
        if self.tarefa.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // título da janela
        let titulo = match &self.doc {
            Some(d) => format!(
                "{}{} — Recorta",
                if d.modificado { "• " } else { "" },
                d.nome()
            ),
            None => "Recorta".into(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(titulo));

        // confirmar antes de fechar com alterações não salvas
        if ctx.input(|i| i.viewport().close_requested())
            && !self.pode_fechar
            && self.doc.as_ref().is_some_and(|d| d.modificado)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirmar_fechar = true;
        }

        egui::Panel::bottom("status").show(ui, |ui| self.barra_status(ui));
        egui::Panel::left("ferramentas")
            .resizable(true)
            .default_size(330.0)
            .size_range(280.0..=520.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.painel(ui));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.canvas(ui));

        if self.confirmar_fechar {
            let modal = egui::Modal::new(egui::Id::unique("fechar")).show(&ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("Alterações não salvas");
                ui.label("A imagem foi modificada. Salvar antes de sair?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("💾 Salvar e sair").clicked() {
                        self.salvar(false);
                        if self.doc.as_ref().is_some_and(|d| !d.modificado) {
                            self.pode_fechar = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        self.confirmar_fechar = false;
                    }
                    if ui.button("Sair sem salvar").clicked() {
                        self.pode_fechar = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Cancelar").clicked() {
                        self.confirmar_fechar = false;
                    }
                });
            });
            if modal.should_close() {
                self.confirmar_fechar = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Geometria da seleção

fn pontos_alcas(s: Rect) -> impl Iterator<Item = Pos2> {
    [
        (-1, -1),
        (0, -1),
        (1, -1),
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
    ]
    .into_iter()
    .map(move |(x, y)| ponto_alca(s, Alca { x, y }))
}

fn ponto_alca(s: Rect, a: Alca) -> Pos2 {
    let x = match a.x {
        -1 => s.min.x,
        0 => s.center().x,
        _ => s.max.x,
    };
    let y = match a.y {
        -1 => s.min.y,
        0 => s.center().y,
        _ => s.max.y,
    };
    pos2(x, y)
}

/// Alça sob o ponto (coordenadas de tela). Cantos têm prioridade.
fn achar_alca(s: Rect, p: Pos2) -> Option<Alca> {
    let raio = 9.0;
    let mut melhor = None;
    let mut dist = f32::MAX;
    for (x, y) in [
        (-1, -1),
        (1, -1),
        (1, 1),
        (-1, 1),
        (0, -1),
        (1, 0),
        (0, 1),
        (-1, 0),
    ] {
        let a = Alca { x, y };
        let d = ponto_alca(s, a).distance(p);
        if d <= raio && d < dist - 0.5 {
            melhor = Some(a);
            dist = d;
        }
    }
    if melhor.is_some() {
        return melhor;
    }
    // bordas inteiras também servem de alça
    let dentro_x = p.x >= s.min.x && p.x <= s.max.x;
    let dentro_y = p.y >= s.min.y && p.y <= s.max.y;
    let perto = 5.0;
    if dentro_x && (p.y - s.min.y).abs() <= perto {
        Some(Alca { x: 0, y: -1 })
    } else if dentro_x && (p.y - s.max.y).abs() <= perto {
        Some(Alca { x: 0, y: 1 })
    } else if dentro_y && (p.x - s.min.x).abs() <= perto {
        Some(Alca { x: -1, y: 0 })
    } else if dentro_y && (p.x - s.max.x).abs() <= perto {
        Some(Alca { x: 1, y: 0 })
    } else {
        None
    }
}

/// Retângulo entre uma âncora fixa e o ponteiro, respeitando proporção e limites.
fn ret_ancora(a: Pos2, p: Pos2, razao: Option<f32>, lim: Vec2) -> Rect {
    let sx = if p.x >= a.x { 1.0 } else { -1.0 };
    let sy = if p.y >= a.y { 1.0 } else { -1.0 };
    let maxw = if sx > 0.0 { lim.x - a.x } else { a.x };
    let maxh = if sy > 0.0 { lim.y - a.y } else { a.y };
    let mut w = (p.x - a.x).abs();
    let mut h = (p.y - a.y).abs();
    if let Some(r) = razao {
        // cresce até cobrir o ponteiro, depois encolhe se passar da borda
        if w / r > h {
            h = w / r
        } else {
            w = h * r
        }
        if w > 0.0 && h > 0.0 {
            let s = (maxw / w).min(maxh / h).min(1.0);
            w *= s;
            h *= s;
        }
    } else {
        w = w.min(maxw);
        h = h.min(maxh);
    }
    Rect::from_two_pos(a, a + vec2(sx * w, sy * h))
}

fn arrastar_alca(alca: Alca, ini: Rect, p: Pos2, razao: Option<f32>, lim: Vec2) -> Rect {
    if alca.x != 0 && alca.y != 0 {
        // canto: âncora no canto oposto
        let ancora = pos2(
            if alca.x < 0 { ini.max.x } else { ini.min.x },
            if alca.y < 0 { ini.max.y } else { ini.min.y },
        );
        return ret_ancora(ancora, p, razao, lim);
    }
    match razao {
        None => {
            let mut r = ini;
            match (alca.x, alca.y) {
                (-1, _) => r.min.x = p.x,
                (1, _) => r.max.x = p.x,
                (_, -1) => r.min.y = p.y,
                _ => r.max.y = p.y,
            }
            Rect::from_two_pos(r.min, r.max)
        }
        Some(razao) => {
            if alca.x != 0 {
                // borda lateral: muda a largura, altura acompanha centrada
                let ax = if alca.x < 0 { ini.max.x } else { ini.min.x };
                let sx = if p.x >= ax { 1.0 } else { -1.0 };
                let cy = ini.center().y;
                let maxw = if sx > 0.0 { lim.x - ax } else { ax };
                let maxh = 2.0 * cy.min(lim.y - cy);
                let w = (p.x - ax).abs().min(maxw).min(maxh * razao);
                let h = w / razao;
                Rect::from_two_pos(pos2(ax, cy - h / 2.0), pos2(ax + sx * w, cy + h / 2.0))
            } else {
                let ay = if alca.y < 0 { ini.max.y } else { ini.min.y };
                let sy = if p.y >= ay { 1.0 } else { -1.0 };
                let cx = ini.center().x;
                let maxh = if sy > 0.0 { lim.y - ay } else { ay };
                let maxw = 2.0 * cx.min(lim.x - cx);
                let h = (p.y - ay).abs().min(maxh).min(maxw / razao);
                let w = h * razao;
                Rect::from_two_pos(pos2(cx - w / 2.0, ay), pos2(cx + w / 2.0, ay + sy * h))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utilidades

fn mdc(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

fn proporcao_txt(w: u32, h: u32) -> String {
    let g = mdc(w, h);
    let (a, b) = (w / g, h / g);
    if a <= 32 && b <= 32 {
        return format!("{a}:{b}");
    }
    let r = w as f32 / h as f32;
    for (_, p) in PROPORCOES {
        if let Proporcao::Fixa(pa, pb) = p {
            let pr = *pa as f32 / *pb as f32;
            if (r / pr - 1.0).abs() < 0.01 {
                return format!("≈{pa}:{pb}");
            }
        }
    }
    format!("{r:.2}:1")
}

fn tipo_txt(c: image::ColorType) -> &'static str {
    use image::ColorType as C;
    match c {
        C::L8 => "cinza 8 bits",
        C::La8 => "cinza+alfa 8 bits",
        C::Rgb8 => "RGB 8 bits",
        C::Rgba8 => "RGBA 8 bits",
        C::L16 => "cinza 16 bits",
        C::La16 => "cinza+alfa 16 bits",
        C::Rgb16 => "RGB 16 bits",
        C::Rgba16 => "RGBA 16 bits",
        C::Rgb32F => "RGB float",
        C::Rgba32F => "RGBA float",
        _ => "outro",
    }
}

fn tamanho_legivel(b: u64) -> String {
    match b {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Decodifica `%XX` de uma URL `file://`. Trabalha em bytes: fatiar a `&str`
/// em `i+1..i+3` daria panic se um `%` viesse antes de um caractere acentuado.
fn decodificar_url(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(&a), Some(&b)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(a), Some(b)) = (hex(a), hex(b))
        {
            out.push(a << 4 | b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn ancora_respeita_proporcao_e_borda() {
        let lim = vec2(100.0, 100.0);
        let r = ret_ancora(pos2(10.0, 10.0), pos2(90.0, 20.0), Some(16.0 / 9.0), lim);
        assert!((r.width() / r.height() - 16.0 / 9.0).abs() < 1e-3);
        assert!(r.max.x <= 100.0 && r.max.y <= 100.0);
        let r = ret_ancora(pos2(50.0, 50.0), pos2(0.0, 0.0), None, lim);
        assert_eq!(r, Rect::from_min_max(pos2(0.0, 0.0), pos2(50.0, 50.0)));
    }

    #[test]
    fn borda_com_proporcao() {
        let lim = vec2(200.0, 100.0);
        let ini = Rect::from_min_max(pos2(10.0, 40.0), pos2(30.0, 60.0));
        let r = arrastar_alca(Alca { x: 1, y: 0 }, ini, pos2(190.0, 50.0), Some(1.0), lim);
        assert!((r.width() - r.height()).abs() < 1e-3);
        assert!(r.min.y >= 0.0 && r.max.y <= 100.0);
    }

    #[test]
    fn textos() {
        assert_eq!(proporcao_txt(1920, 1080), "16:9");
        assert_eq!(proporcao_txt(1366, 768), "≈16:9");
        assert_eq!(proporcao_txt(20, 20), "1:1");
        assert_eq!(decodificar_url("/tmp/a%20b.png"), "/tmp/a b.png");
        assert_eq!(decodificar_url("/tmp/a%C3%A7%C3%A3o.png"), "/tmp/ação.png");
    }

    #[test]
    fn url_com_porcento_solto_nao_quebra() {
        // antes: panic ao fatiar a string no meio do "€" / "é"
        assert_eq!(decodificar_url("/tmp/50%€.png"), "/tmp/50%€.png");
        assert_eq!(decodificar_url("/tmp/%aé.png"), "/tmp/%aé.png");
        assert_eq!(decodificar_url("/tmp/100%"), "/tmp/100%");
        assert_eq!(decodificar_url("/tmp/%zz%4"), "/tmp/%zz%4");
    }
}
