# packaging/

Everything here except this file is **generated**.
The source is the binary, not the tree:

    memory-industry setup service --print --linux  --out packaging
    memory-industry setup service --print --windows --out packaging

`rust/tests/packaging_contract.rs` compares the tree against that render byte for
byte, so editing a file here by hand fails the gate — and the failure prints the
line that differs and the command above. That is the point: a boot artifact
written by hand is a second source of truth for the defaults, and that is how a
hand-pinned GPU ceiling ended up beating the resource planner's measurement on
every machine with a card.

The files under `packaging/windows/` are **documentation of the shape**, not
something to hand to `schtasks`: they write `%LOCALAPPDATA%` and `%USERPROFILE%`
because the render has no machine to name. The scheduled task runs as SYSTEM,
which cannot expand either (it has no profile of its own), so installing these
files as they are would register a task pointing at an empty folder. They are
installed with `setup service --apply` (add `--register` on Windows, from an
elevated shell), which renders the same files with the absolute paths of the
machine it runs on.

The units carry **no knobs**. They point at an env file, and that file offers
every key the planner can set, commented and without a value — the planner
measures them at startup and a value written there wins for good.

`cuba-memorys.service` and `cuba-memorys.socket`, the hand-written pair that
carried the defect, left the tree in 0.28.0. `--uninstall` does not remove a
copy of them already installed; that is `systemctl --user disable --now` on both
and deleting the two files by hand.

Install, uninstall and what `--apply` will not overwrite:

    memory-industry setup service              # the plan, writes nothing
    memory-industry setup service --apply      # installs; keeps an existing env file
    memory-industry setup service --uninstall  # removes the units, keeps the env file

Repository: https://github.com/LeandroPG19/Memorys
