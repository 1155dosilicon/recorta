#!/bin/sh
# Instala o Recorta para o usuário atual (sem root):
#   ~/.local/bin/recorta, ícone e atalho no menu de aplicativos.
set -eu
cd "$(dirname "$0")"

BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
DADOS="${XDG_DATA_HOME:-$HOME/.local/share}"

if [ -x target/release/recorta ] && [ ! -f recorta ]; then
    ORIGEM=target/release/recorta   # rodando de dentro do código-fonte
else
    ORIGEM=recorta
fi
[ -f "$ORIGEM" ] || { echo "binário 'recorta' não encontrado (compile com: cargo build --release)" >&2; exit 1; }
[ -d assets ] && A=assets || A=.

ICONES="$DADOS/icons/hicolor"
mkdir -p "$BIN" "$DADOS/applications" "$ICONES/scalable/apps" "$ICONES/64x64/apps"
# instala num nome temporário e renomeia: funciona mesmo com o Recorta aberto
install -m755 "$ORIGEM" "$BIN/.recorta.novo"
mv -f "$BIN/.recorta.novo" "$BIN/recorta"
install -m644 "$A/recorta.svg" "$ICONES/scalable/apps/recorta.svg"
# PNG de reserva para painéis/temas que não desenham SVG
[ -f "$A/recorta-64.png" ] && install -m644 "$A/recorta-64.png" "$ICONES/64x64/apps/recorta.png"
# caminho completo no Exec: o menu e o gerenciador de arquivos costumam rodar com um
# PATH sem ~/.local/bin, e aí o Whisker esconde o atalho e o "Abrir com" falha
sed "s|^Exec=recorta |Exec=$BIN/recorta |" "$A/recorta.desktop" > "$DADOS/applications/recorta.desktop"
chmod 644 "$DADOS/applications/recorta.desktop"

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$DADOS/applications" || true
# O GTK só compara a data do icon-theme.cache com a da pasta hicolor/ (não das
# subpastas). Se já existe um cache de outro programa, o ícone novo fica invisível
# no menu até o cache ser refeito. Atualiza a data da pasta (invalida o cache
# antigo mesmo sem gtk-update-icon-cache) e refaz o cache se der.
touch "$ICONES"
if [ -f "$ICONES/icon-theme.cache" ] || [ -f "$ICONES/index.theme" ]; then
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -f -t "$ICONES" 2>/dev/null || true
fi

echo "Recorta instalado em $BIN/recorta"
case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo "Aviso: $BIN não está no PATH. Abra pelo menu de aplicativos ou adicione-o ao PATH." ;;
esac
