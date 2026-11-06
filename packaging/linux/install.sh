#!/bin/sh
# Installs PulseTerm for this user -- no root, nothing outside your home.
#
#   packaging/linux/install.sh                    the release build if there is one, else debug
#   packaging/linux/install.sh --debug            the debug build
#   packaging/linux/install.sh --default-terminal also make it the terminal other programs open
#   packaging/linux/install.sh --uninstall        remove what this put in place
#
# What goes where:
#   ~/.local/bin/pulseterm                                  the program
#   ~/.local/bin/pulseterm-remote                           the remote desktop helper, if built
#   ~/.local/share/icons/hicolor/scalable/apps/pulseterm.svg
#   ~/.local/share/applications/pulseterm.desktop           the menu entry
#
# The binaries are copied, not linked, so a rebuild in progress never swaps a
# running copy out from under itself; run this again after rebuilding.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
bin="$HOME/.local/bin"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
terminals="${XDG_CONFIG_HOME:-$HOME/.config}/xdg-terminals.list"

profile=""
default_terminal=no
for arg in "$@"; do
  case "$arg" in
    --debug) profile=debug ;;
    --release) profile=release ;;
    --default-terminal) default_terminal=yes ;;
    --uninstall)
      rm -f "$bin/pulseterm" "$bin/pulseterm-remote" "$icons/pulseterm.svg" "$apps/pulseterm.desktop"
      if [ -f "$terminals" ]; then sed -i '/^pulseterm\.desktop$/d' "$terminals"; fi
      command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
      echo "PulseTerm removed."
      exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [ -z "$profile" ]; then
  if [ -x "$repo/target/release/PulseTerm" ]; then profile=release; else profile=debug; fi
fi
src="$repo/target/$profile/PulseTerm"
if [ ! -x "$src" ]; then
  if [ "$profile" = release ]; then hint="cargo build --release"; else hint="cargo build"; fi
  echo "No $profile build at $src -- run: $hint" >&2
  exit 1
fi

mkdir -p "$bin" "$apps" "$icons"
install -m 755 "$src" "$bin/pulseterm"
# The helper is found beside the program at run time; without it the remote
# desktop icon simply stays hidden.
if [ -x "$repo/target/$profile/pulseterm-remote" ]; then
  install -m 755 "$repo/target/$profile/pulseterm-remote" "$bin/pulseterm-remote"
fi
install -m 644 "$repo/assets/logo.svg" "$icons/pulseterm.svg"
# Exec and TryExec get the full path: a desktop session's PATH is not your
# shell's, and ~/.local/bin is often only added by ~/.bashrc. A bare
# `pulseterm` then makes launchers hide the entry (TryExec) or fail to start it.
sed -e "s|^Exec=pulseterm|Exec=$bin/pulseterm|" -e "s|^TryExec=pulseterm|TryExec=$bin/pulseterm|" \
  "$repo/packaging/linux/pulseterm.desktop" > "$apps/pulseterm.desktop"
chmod 644 "$apps/pulseterm.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" || true

# Optional: first in xdg-terminal-exec's list, which is what "open in a
# terminal" in other programs asks. Other entries stay, after it.
if [ "$default_terminal" = yes ]; then
  mkdir -p "$(dirname "$terminals")"
  touch "$terminals"
  sed -i '/^pulseterm\.desktop$/d' "$terminals"
  sed -i '1i pulseterm.desktop' "$terminals" 2>/dev/null || echo pulseterm.desktop > "$terminals"
  [ -s "$terminals" ] || echo pulseterm.desktop > "$terminals"
fi

case ":$PATH:" in
  *":$bin:"*) ;;
  *) echo "Note: $bin is not on your PATH; the menu entry works, the 'pulseterm' command will not." ;;
esac
# The compositor's PATH is the one a keyboard shortcut (niri's `spawn`, sway's
# `exec`) searches, and it usually comes from the systemd user session.
if command -v systemctl >/dev/null; then
  case ":$(systemctl --user show-environment 2>/dev/null | sed -n 's/^PATH=//p'):" in
    *":$bin:"*|"::") ;;
    *) echo "Note: your desktop session's PATH lacks $bin; bind shortcuts to $bin/pulseterm, not pulseterm." ;;
  esac
fi
echo "PulseTerm ($profile build) installed. Find it in your applications menu, or run: pulseterm"
