fix:
    cargo fmt
    cargo sort --workspace
    cargo machete --fix

check:
    cargo audit
    cargo fmt --all -- --check
    cargo sort --check
    cargo machete

test:
    cargo nextest run --all-features --features fastrace/enable
