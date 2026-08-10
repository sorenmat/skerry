on open fileList
	set novaBinary to POSIX path of (path to resource "nova")
	set commandLine to quoted form of novaBinary
	repeat with f in fileList
		set commandLine to commandLine & " " & quoted form of (POSIX path of f)
	end repeat
	do shell script commandLine & " >/dev/null 2>&1 &"
end open

on run
	set novaBinary to POSIX path of (path to resource "nova")
	do shell script quoted form of novaBinary & " >/dev/null 2>&1 &"
end run
