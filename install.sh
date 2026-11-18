#!/usr/bin/env sh
set -eu

REPO="oliverduds/skillctl"
BIN_NAME="skillctl"
INSTALL_DIR="${SKILLCTL_INSTALL_DIR:-$HOME/.local/bin}"

say() {
  printf '%s\n' "$*"
}

err() {
  printf 'Erro: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || err "Comando obrigatório não encontrado: $1"
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64|amd64)  echo "x86_64-apple-darwin" ;;
        *) err "Arquitetura macOS não suportada: $arch" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64|amd64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
        *) err "Arquitetura Linux não suportada: $arch" ;;
      esac
      ;;
    *)
      err "Sistema operacional não suportado por este script ($os). No Windows use install.ps1."
      ;;
  esac
}

ensure_path() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) return 0 ;;
  esac

  export_line="export PATH=\"$INSTALL_DIR:\$PATH\""
  shell_name="$(basename "${SHELL:-sh}")"
  profile=""
  case "$shell_name" in
    zsh)  profile="$HOME/.zshrc" ;;
    bash)
      if [ -f "$HOME/.bashrc" ]; then
        profile="$HOME/.bashrc"
      else
        profile="$HOME/.bash_profile"
      fi
      ;;
    *)    profile="$HOME/.profile" ;;
  esac

  if [ -n "$profile" ] && [ -w "$(dirname "$profile")" ]; then
    if ! grep -qs "$INSTALL_DIR" "$profile" 2>/dev/null; then
      printf '\n# skillctl\n%s\n' "$export_line" >> "$profile"
      say "✓ Adicionado $INSTALL_DIR ao PATH em $profile"
    fi
  fi
  say "  Para usar nesta sessão, execute: $export_line"
}

main() {
  need_cmd curl
  need_cmd tar

  target="$(detect_target)"
  version="${SKILLCTL_VERSION:-latest}"

  if [ "$version" = "latest" ]; then
    base_url="https://github.com/$REPO/releases/latest/download"
  else
    base_url="https://github.com/$REPO/releases/download/$version"
  fi

  archive="${BIN_NAME}-${target}.tar.gz"
  url="${base_url}/${archive}"
  sums_url="${base_url}/SHA256SUMS.txt"

  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT INT TERM

  say "Baixando ${BIN_NAME} (${target})..."
  if ! curl -fsSL "$url" -o "$tmpdir/$archive"; then
    err "Não foi possível baixar $url. Verifique se a release já terminou de ser publicada em https://github.com/$REPO/releases"
  fi

  # Verify SHA-256 when checksum file and hash tool are available
  if curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS.txt" 2>/dev/null; then
    expected="$(grep " ${archive}\$" "$tmpdir/SHA256SUMS.txt" | awk '{print $1}' || true)"
    if [ -n "$expected" ]; then
      if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$tmpdir/$archive" | awk '{print $1}')"
      elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$tmpdir/$archive" | awk '{print $1}')"
      else
        actual=""
      fi
      if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
        err "Falha na verificação SHA-256 de $archive (esperado $expected, obtido $actual)"
      fi
    fi
  fi

  mkdir -p "$INSTALL_DIR"
  tar -xzf "$tmpdir/$archive" -C "$tmpdir"
  chmod +x "$tmpdir/$BIN_NAME"
  mv "$tmpdir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"

  say "✓ ${BIN_NAME} instalado em $INSTALL_DIR/$BIN_NAME"
  ensure_path
  say ""
  say "Pronto! Execute \`skillctl\` no terminal para abrir o menu interativo."
}

main "$@"
