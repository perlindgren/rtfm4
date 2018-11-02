set -euxo pipefail

main() {
    rm -f .cargo/config
    cargo doc --features timer-queue
    ( cd book && mdbook build )

    local td=$(mktemp -d)
    cp -r target/doc $td/api
    cp -r book/book $td/

    echo $td
    # TODO uncomment
    # rm -rf $td
}

main
