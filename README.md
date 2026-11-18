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

```sh
cargo build --release -j10
install -m755 target/release/recorta ~/.local/bin/
install -Dm644 assets/recorta.svg ~/.local/share/icons/hicolor/scalable/apps/recorta.svg
install -Dm644 assets/recorta.desktop ~/.local/share/applications/recorta.desktop
```
