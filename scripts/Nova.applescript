on open fileList
	repeat with f in fileList
		set posixPath to POSIX path of f
		do shell script "/Users/smo/code/the_editor/target/release/frontend_gui " & quoted form of posixPath & " >/dev/null 2>&1 &"
	end repeat
end open

on run
	do shell script "/Users/smo/code/the_editor/target/release/frontend_gui >/dev/null 2>&1 &"
end run
