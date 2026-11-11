#!/usr/bin/env bash
# Shared agent skill directory map for ghostless.
# Paths match the network-jobs harness list: Claude Code, Cursor, Codex,
# OpenCode, Pi, Gemini CLI, Hermes, Muse, and the generic agents dir.
# shellcheck shell=bash

GHOSTLESS_AGENT_IDS=(
  claude-code
  cursor
  codex
  opencode
  pi
  gemini-cli
  hermes-agent
  muse
  agents
)

GHOSTLESS_SKILL_IDS=(
  ghostless-draft
  ghostless-edit
  ghostless-learn
)

ghostless_agent_dir() {
  local name="$1"
  case "$name" in
    claude-code) echo "$HOME/.claude/skills" ;;
    cursor) echo "$HOME/.cursor/skills" ;;
    codex) echo "$HOME/.codex/skills" ;;
    opencode) echo "$HOME/.config/opencode/skills" ;;
    pi) echo "$HOME/.pi/agent/skills" ;;
    gemini-cli) echo "$HOME/.gemini/skills" ;;
    hermes-agent) echo "$HOME/.hermes/skills" ;;
    muse) echo "$HOME/.agents/skills" ;;
    agents) echo "$HOME/.agents/skills" ;;
    *) return 1 ;;
  esac
}

ghostless_agent_hint() {
  local name="$1"
  case "$name" in
    claude-code) echo "$HOME/.claude" ;;
    cursor) echo "$HOME/.cursor" ;;
    codex) echo "$HOME/.codex" ;;
    opencode) echo "$HOME/.config/opencode" ;;
    pi) echo "$HOME/.pi" ;;
    gemini-cli) echo "$HOME/.gemini" ;;
    hermes-agent) echo "$HOME/.hermes" ;;
    muse) echo "$HOME/.agents" ;;
    agents) echo "$HOME/.agents" ;;
    *) return 1 ;;
  esac
}

# Some harnesses keep no home directory. A binary on PATH counts.
ghostless_agent_bin() {
  local name="$1"
  case "$name" in
    muse) echo "muse" ;;
    *) return 1 ;;
  esac
}

ghostless_agent_detected() {
  local name="$1" hint bin
  hint="$(ghostless_agent_hint "$name" || true)"
  if [[ -n "$hint" && -d "$hint" ]]; then
    return 0
  fi
  bin="$(ghostless_agent_bin "$name" || true)"
  if [[ -n "$bin" ]] && command -v "$bin" >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

ghostless_detect_agents() {
  local name
  local -a found=()
  for name in "${GHOSTLESS_AGENT_IDS[@]}"; do
    if ghostless_agent_detected "$name"; then
      found+=("$name")
    fi
  done
  if ((${#found[@]} == 0)); then
    found=("claude-code")
  fi
  printf '%s\n' "${found[@]}"
}

ghostless_list_agents() {
  local name dir
  for name in "${GHOSTLESS_AGENT_IDS[@]}"; do
    dir="$(ghostless_agent_dir "$name")"
    if ghostless_agent_detected "$name"; then
      printf '%s\t%s\tdetected\n' "$name" "$dir"
    else
      printf '%s\t%s\t\n' "$name" "$dir"
    fi
  done
}

ghostless_is_npm_global_install() {
  local root="${1:-}"
  [[ -z "$root" ]] && return 1
  case "$root" in
    */lib/node_modules/ghostless|*/lib/node_modules/ghostless/) return 0 ;;
    *) return 1 ;;
  esac
}

ghostless_is_packaged_install() {
  local root="${1:-}"
  [[ -z "$root" ]] && return 1
  [[ -d "$root/.git" ]] && return 1
  case "$root" in
    */node_modules/ghostless|*/node_modules/ghostless/) return 0 ;;
    */.local/share/pnpm/*) return 0 ;;
    */.npm/_npx/*) return 0 ;;
    */.cache/pnpm/*) return 0 ;;
    *) return 1 ;;
  esac
}

ghostless_launcher_candidates() {
  printf '%s\n' "$HOME/.local/bin" "$HOME/bin"
}

ghostless_launcher_dir() {
  local candidate
  while IFS= read -r candidate; do
    case ":$PATH:" in
      *":$candidate:"*) printf '%s\n' "$candidate"; return 0 ;;
    esac
  done < <(ghostless_launcher_candidates)
  ghostless_launcher_candidates | head -n 1
}

ghostless_launcher_paths() {
  local dir path
  while IFS= read -r dir; do
    path="$dir/ghostless"
    if [[ -f "$path" && ! -L "$path" ]] && grep -q "ghostless launcher" "$path" 2>/dev/null; then
      printf '%s\n' "$path"
    fi
  done < <(ghostless_launcher_candidates)
  return 0
}

ghostless__cache_entry_root() {
  local base="$1" nested="$2" rest
  rest="${nested#"$base"/}"
  printf '%s/%s\n' "$base" "${rest%%/*}"
}

ghostless_purge_package_cache() {
  local removed=0 cache entry nested
  local -a entries=()

  for cache in "$HOME"/.local/share/pnpm/store/v*/links/ghostless; do
    if [[ -e "$cache" || -L "$cache" ]]; then
      rm -rf "$cache"
      echo "removed $cache"
      removed=1
    fi
  done

  for cache in "$HOME/.cache/pnpm/dlx" "$HOME/.npm/_npx"; do
    [[ -d "$cache" ]] || continue
    entries=()
    while IFS= read -r nested; do
      entries+=("$(ghostless__cache_entry_root "$cache" "$nested")")
    done < <(find "$cache" -maxdepth 6 -type d -path '*node_modules/ghostless' 2>/dev/null || true)
    ((${#entries[@]})) || continue
    while IFS= read -r entry; do
      [[ -d "$entry" ]] || continue
      rm -rf "$entry"
      echo "removed cached copy $entry"
      removed=1
    done < <(printf '%s\n' "${entries[@]}" | sort -u)
  done

  ((removed)) || echo "no cached suite copies found"
}

ghostless_trim() {
  local value="$1"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s' "$value"
}

ghostless_clear_dangling_suite_root() {
  local data="${GHOSTLESS_HOME:-$HOME/.ghostless}"
  local file="$data/suite-root" root
  [[ -f "$file" ]] || return 0
  root="$(ghostless_trim "$(cat "$file" 2>/dev/null || true)")"
  [[ -z "$root" ]] && return 0
  [[ -x "$root/bin/ghostless" ]] && return 0
  rm -f "$file"
  printf 'cleared dangling suite-root: %q\n' "$root"
  return 0
}

ghostless_package_runner() {
  if command -v pnpm >/dev/null 2>&1 && pnpm --version >/dev/null 2>&1; then
    echo "pnpm dlx"
    return 0
  fi
  if command -v npx >/dev/null 2>&1 && npx --version >/dev/null 2>&1; then
    echo "npx --yes"
    return 0
  fi
  return 1
}

ghostless_setup_rerun_command() {
  if command -v npx >/dev/null 2>&1; then
    echo "npx --yes ghostless@latest install-skills"
  elif command -v pnpm >/dev/null 2>&1; then
    echo "pnpm dlx ghostless@latest install-skills"
  else
    echo "npm i -g ghostless@latest && ghostless install-skills"
  fi
}
