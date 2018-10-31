set -euxo pipefail

main() {
    local T=$TARGET

    if [ $T = x86_64-unknown-linux-gnu ]; then
        cargo test --test compiletest --target $T
        cargo check --target $T
        return
    fi

    cargo check --target $T --examples

    case $T in
        thumbv6m-none-eabi | thumbv7m-none-eabi)
            local exs=(
                idle
                init
                interrupt

                resource
                lock
                late

                task
                message
                capacity

                singleton
            )

            for ex in ${exs[@]}; do
                cargo run --example $ex --target $T | diff -u ci/expected/$ex.run -
            done

            if [ $T != thumbv6m-none-eabi ]; then
                cargo run --example ramfunc --target $T --release | diff -u ci/expected/ramfunc.run -
            fi
            ;;
    esac

}

main
