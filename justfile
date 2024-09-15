fix:
    cargo fmt
    cargo sort --workspace
    cargo machete --fix

check:
    cargo fmt --all -- --check
    cargo sort --check
    cargo machete
