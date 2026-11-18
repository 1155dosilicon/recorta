# Recorta

Editor de imagens simples em Rust (egui + fast_image_resize).

- **Recortar** com proporções 1:1, 4:3, 16:9, 9:16, 3:2, 21:9… ou livre/personalizada (sem perdas).
- **Redimensionar** para qualquer largura × altura (estica/achata), ex.: 200×10, 512×1048.
- **Escala** mantendo a proporção, ex.: 20×20 → 500×500, com Lanczos3, Catmull-Rom, Mitchell,
  vizinho mais próximo ou Scale2x/3x para pixel art; nitidez e luz linear opcionais.
- Girar/espelhar, desfazer/refazer, copiar/colar imagem, arrastar arquivos, 8/16 bits preservados.

Atalhos: Ctrl+O abrir · Ctrl+S salvar · Ctrl+Shift+S salvar como · Ctrl+Z/Ctrl+Shift+Z ·
Enter aplica o recorte · Esc cancela · roda do mouse = zoom · botão do meio/direito ou Espaço+arrastar = mover ·
Ctrl+0 ajustar · Ctrl+1 100% · Ctrl+C/Ctrl+V.

## Instalar

Baixe o `recorta-<versão>-linux-x86_64.tar.gz` em [Releases](https://github.com/1155dosilicon/recorta/releases/latest) e rode:

```sh
tar xzf recorta-*-linux-x86_64.tar.gz
sh recorta-*-linux-x86_64/instalar.sh
```

Instala em `~/.local/bin/recorta`, com ícone e atalho no menu (sem root). O binário roda em qualquer
Linux x86_64 com glibc 2.31 ou mais nova (Ubuntu 20.04+, Debian 11+, Mint 20+, Fedora, Arch…),
em X11 ou Wayland, com OpenGL (qualquer driver, inclusive o Mesa por software).

## Compilar

```sh
cargo build --release
sh instalar.sh             # instala o binário de target/release
sh scripts/release.sh      # pacote portátil em dist/ (compila no Ubuntu 20.04 via podman)
```
