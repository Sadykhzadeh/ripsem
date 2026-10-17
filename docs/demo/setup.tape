# The setup demo in the README: hand ripsem to a coding agent, build the
# index, and ask it something.
#
# Needs ripsem on PATH, the embedding model already downloaded, and
# $RIPSEM_DEMO/mux holding gorilla/mux v1.8.1. From the repository root:
# sh docs/demo/record.sh

Output docs/demo/setup.gif

Set Shell bash
Set FontSize 15
Set Width 1180
Set Height 470
Set Padding 24
Set Theme "GitHub Dark"
Set TypingSpeed 35ms
Set WaitPattern /^\$$/
Set WaitTimeout 120s

Hide
Type `export PS1='$ ' && cd "$RIPSEM_DEMO/mux" && rm -rf .ripsem .mcp.json CLAUDE.md && clear`
Enter
Wait
Show

Sleep 600ms
Type "ripsem install claude-code"
Sleep 400ms
Enter
Wait
Sleep 2.5s

Type "ripsem index"
Sleep 400ms
Enter
Wait
Sleep 1.5s

Type `ripsem search "walk every registered route" -k 2 --context 1`
Sleep 400ms
Enter
Wait
Sleep 5s
