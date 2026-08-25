# TODO

- 2026-08-25 13:25: Find out why the NSIS installer hangs on /S. It hung again on a clean single run with no other instance and no installer process racing it, left the installed binary untouched, and had to be killed after four minutes. Reinstalling by copying target/release/openhistory-win.exe over %LOCALAPPDATA%/OpenHistory works, but a silent install that never returns blocks any real release. It has now failed twice and succeeded once, so it is intermittent rather than always broken.
