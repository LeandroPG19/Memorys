# packaging/

Everything here except this file and the two `cuba-memorys.*` is **generated**.
The source is the binary, not the tree:

    memory-industry setup service --print --linux  --out packaging
    memory-industry setup service --print --windows --out packaging

`rust/tests/packaging_contract.rs` compares the tree against that render byte for
byte, so editing a file here by hand fails the gate — and the failure prints the
line that differs and the command above. That is the point: a boot artifact
written by hand is a second source of truth for the defaults, and that is how a
hand-pinned GPU ceiling ended up beating the resource planner's measurement on
every machine with a card.

The units carry **no knobs**. They point at an env file, and that file offers
every key the planner can set, commented and without a value — the planner
measures them at startup and a value written there wins for good.

`cuba-memorys.service` and `cuba-memorys.socket` are the previous release's
names, kept for one version and dropped at 0.28.0. They still carry the defect;
they are exempted by name, with an expiry the contract enforces on its own.

Install, uninstall and what `--apply` will not overwrite:

    memory-industry setup service              # the plan, writes nothing
    memory-industry setup service --apply      # installs; keeps an existing env file
    memory-industry setup service --uninstall  # removes the units, keeps the env file

Repository: https://github.com/LeandroPG19/Memorys
