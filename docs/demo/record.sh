#!/bin/sh
# Records every tape here in GitHub's dark theme and again in its light one,
# as <name>.gif and <name>-light.gif. Run from the repository root with vhs on
# PATH; each tape says what else it needs.
set -e
light_theme='Set Theme { "name": "GitHub Light", "background": "#ffffff", "foreground": "#1f2328", "selection": "#b6e3ff", "cursor": "#0969da", "black": "#24292f", "red": "#cf222e", "green": "#116329", "yellow": "#4d2d00", "blue": "#0969da", "purple": "#8250df", "cyan": "#1b7c83", "white": "#6e7781", "brightBlack": "#57606a", "brightRed": "#a40e26", "brightGreen": "#1a7f37", "brightYellow": "#633c01", "brightBlue": "#218bff", "brightPurple": "#a475f9", "brightCyan": "#3192aa", "brightWhite": "#8c959f" }'
light="${TMPDIR:-/tmp}/ripsem-light-$$.tape"
trap 'rm -f "$light"' EXIT
for tape in docs/demo/*.tape; do
  vhs "$tape"
  awk -v theme="$light_theme" '
    /^Set Theme / { print theme; next }
    /^Output .*\.gif$/ { sub(/\.gif$/, "-light.gif") }
    { print }
  ' "$tape" > "$light"
  vhs "$light"
done
