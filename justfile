default:
  just --list

fix:
    cargo fmt
    cargo sort --workspace
    cargo machete --fix

check:
    cargo fmt --all -- --check
    cargo sort --check
    cargo machete

test:
    cargo nextest run --all-features --features fastrace/enable

install-pre-commit:
    (! (grep -q 'just check' .git/hooks/pre-commit 2> /dev/null && echo 'You have installed.') && echo -n 'just check' >> .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit && echo 'Installed!') || true

remove-pre-commit:
    (! (sed -i.bak 's/just check//g' .git/hooks/pre-commit 2> /dev/null && echo 'Removed') && echo 'No pre-commit file found') || true
