on open fileList
	set skerryBinary to POSIX path of (path to resource "skerry")
	set commandLine to quoted form of skerryBinary
	repeat with f in fileList
		set commandLine to commandLine & " " & quoted form of (POSIX path of f)
	end repeat
	do shell script commandLine & " >/dev/null 2>&1 &"
end open

on run
	set skerryBinary to POSIX path of (path to resource "skerry")
	do shell script quoted form of skerryBinary & " >/dev/null 2>&1 &"
end run
