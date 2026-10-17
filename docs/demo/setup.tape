# The setup demo in the README: index a repository, check it, and hand the
# search to a coding agent.
#
# Needs ripsem on PATH, the embedding model already downloaded, and
# $RIPSEM_DEMO/mux holding gorilla/mux v1.8.1. From the repository root:
# vhs docs/demo/setup.tape

Output docs/demo/setup.gif

Set Shell bash
Set FontSize 15
Set Width 1180
Set Height 490
Set Padding 24
Set Theme "GitHub Dark"
Set TypingSpeed 35ms
Set WaitPattern /^\$$/
Set WaitTimeout 120s

Hide
Type `export PS1='$ ' && cd "$RIPSEM_DEMO/mux" && rm -rf .ripsem .mcp.json && clear`
Enter
Wait
Show

Sleep 600ms
Type "ripsem index"
Sleep 400ms
Enter
Wait
Sleep 1.5s

Type "ripsem status"
Sleep 400ms
Enter
Wait
Sleep 2.5s

Type "ripsem install claude-code"
Sleep 400ms
Enter
Wait
Sleep 1.5s

Type "cat .mcp.json"
Sleep 400ms
Enter
Wait
Sleep 4s
