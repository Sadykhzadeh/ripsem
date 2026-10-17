# The search demo in the README: a description, an error message and a
# misremembered name, each answered by the right function.
#
# Needs ripsem on PATH and $RIPSEM_DEMO/requests holding psf/requests v2.34.2,
# already indexed. From the repository root: vhs docs/demo/search.tape

Output docs/demo/search.gif

Set Shell bash
Set FontSize 15
Set Width 1180
Set Height 590
Set Padding 24
Set Theme "GitHub Dark"
Set TypingSpeed 35ms
Set WaitPattern /^\$$/

Hide
Type `export PS1='$ ' && cd "$RIPSEM_DEMO/requests" && clear`
Enter
Wait
Show

Sleep 600ms
Type `ripsem search "strip the auth header when redirecting to another host" -k 2 --context 2`
Sleep 400ms
Enter
Wait
Sleep 3.5s

Type `ripsem search "Exceeded 30 redirects" -k 2 --context 2`
Sleep 400ms
Enter
Wait
Sleep 3.5s

Type `ripsem search "should_strip_authorization" -k 2 --context 2`
Sleep 400ms
Enter
Wait
Sleep 5s
