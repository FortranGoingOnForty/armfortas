#!/bin/sh
# Shared fail-closed corpus runner for the x86 ISA safety gates.
#
# The counts are deliberate evidence contracts. Update them in the same change
# as any test_programs/*.f90 addition/removal or ERROR_EXPECTED reclassification.
isa_gate_expected_sources=878
isa_gate_expected_diagnostics=125

isa_gate_validate_assembly() {
    if grep -nE '^[[:space:]]*\.arch([[:space:]]|$)' "$1"; then
        echo "$isa_gate_name: generated assembly may not override the gate architecture" >&2
        return 1
    else
        isa_gate_grep_status=$?
        if [ "$isa_gate_grep_status" -ne 1 ]; then
            return "$isa_gate_grep_status"
        fi
    fi

    if printf '%s\n' "$isa_gate_arch_prelude" |
        "$isa_gate_scanner" --64 -o "$1.isa-gate.o" - "$1"; then
        return 0
    else
        isa_gate_scanner_status=$?
        return "$isa_gate_scanner_status"
    fi
}

isa_gate_run() {
    if [ "$#" -gt 1 ]; then
        echo "$isa_gate_name: usage: $0 [path-to-armfortas]" >&2
        return 2
    fi

    isa_gate_compiler="${1:-${CARGO_TARGET_DIR:-target}/debug/armfortas}"
    if [ ! -x "$isa_gate_compiler" ]; then
        echo "$isa_gate_name: compiler not found at $isa_gate_compiler" >&2
        return 2
    fi
    if ! command -v "$isa_gate_scanner" >/dev/null 2>&1; then
        echo "$isa_gate_name: policy checker not found: $isa_gate_scanner" >&2
        return 2
    fi

    isa_gate_root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
    isa_gate_corpus="$isa_gate_root/test_programs"
    set -- "$isa_gate_corpus"/*.f90
    if [ "$#" -eq 1 ] && [ ! -f "$1" ]; then
        echo "$isa_gate_name: no .f90 sources found under $isa_gate_corpus" >&2
        return 2
    fi

    isa_gate_sources=$#
    if [ "$isa_gate_sources" -ne "$isa_gate_expected_sources" ]; then
        echo "$isa_gate_name: expected $isa_gate_expected_sources .f90 sources, found $isa_gate_sources" >&2
        return 1
    fi

    isa_gate_tmpdir=$(mktemp -d)
    trap 'rm -rf "$isa_gate_tmpdir"' EXIT
    trap 'exit 2' HUP INT TERM

    isa_gate_allowed_path="$isa_gate_tmpdir/allowed-probe.s"
    printf '.text\n%s\nret\n' "$isa_gate_allowed_probe" >"$isa_gate_allowed_path"
    if ! isa_gate_validate_assembly "$isa_gate_allowed_path" \
        >"$isa_gate_tmpdir/allowed-probe.stdout" \
        2>"$isa_gate_tmpdir/allowed-probe.stderr"; then
        echo "$isa_gate_name: policy checker rejected its allowed-instruction control" >&2
        sed -n '1,20{s/^/  /;p;}' "$isa_gate_tmpdir/allowed-probe.stdout" >&2
        sed -n '1,20{s/^/  /;p;}' "$isa_gate_tmpdir/allowed-probe.stderr" >&2
        return 2
    fi

    isa_gate_forbidden_path="$isa_gate_tmpdir/forbidden-probe.s"
    printf '.text\n%s\nret\n' "$isa_gate_forbidden_probe" >"$isa_gate_forbidden_path"
    if isa_gate_validate_assembly "$isa_gate_forbidden_path" \
        >"$isa_gate_tmpdir/forbidden-probe.stdout" \
        2>"$isa_gate_tmpdir/forbidden-probe.stderr"; then
        echo "$isa_gate_name: policy checker accepted its forbidden-instruction control" >&2
        return 2
    else
        isa_gate_probe_status=$?
        if [ "$isa_gate_probe_status" -ne 1 ]; then
            echo "$isa_gate_name: forbidden-instruction control failed unexpectedly (exit $isa_gate_probe_status)" >&2
            sed -n '1,20{s/^/  /;p;}' "$isa_gate_tmpdir/forbidden-probe.stdout" >&2
            sed -n '1,20{s/^/  /;p;}' "$isa_gate_tmpdir/forbidden-probe.stderr" >&2
            return 2
        fi
    fi

    isa_gate_levels_count=0
    for isa_gate_level in $isa_gate_levels; do
        isa_gate_levels_count=$((isa_gate_levels_count + 1))
    done

    isa_gate_diagnostics=0
    isa_gate_attempted=0
    isa_gate_checked=0
    isa_gate_compile_failures=0
    isa_gate_output_failures=0
    isa_gate_scan_failures=0
    isa_gate_hits=0
    isa_gate_setup_failures=0

    for isa_gate_source do
        isa_gate_base=$(basename "$isa_gate_source" .f90)

        # Diagnostic fixtures are required to reject and therefore cannot
        # produce assembly. run_programs validates their exact diagnostics.
        if grep -Eq '^[[:space:]]*![[:space:]]*ERROR_EXPECTED:' "$isa_gate_source"; then
            isa_gate_diagnostics=$((isa_gate_diagnostics + 1))
            continue
        fi

        isa_gate_source_flag=$(
            sed -n '
                s/^[[:space:]]*![[:space:]]*FLAGS:[[:space:]]*//
                t found
                b
                :found
                s/[[:space:]]*$//
                p
            ' "$isa_gate_source"
        )
        case "$isa_gate_source_flag" in
            "" | --std=f2018 | --std=f2023 | -fcheck=bounds | -fdefault-integer-8) ;;
            *)
                echo "$isa_gate_name: unsupported or repeated FLAGS annotation in $isa_gate_base: $isa_gate_source_flag" >&2
                isa_gate_setup_failures=$((isa_gate_setup_failures + 1))
                continue
                ;;
        esac

        for isa_gate_level in $isa_gate_levels; do
            isa_gate_out="$isa_gate_tmpdir/$isa_gate_base$isa_gate_level.s"
            isa_gate_err="$isa_gate_tmpdir/$isa_gate_base$isa_gate_level.stderr"
            isa_gate_module_dir="$isa_gate_tmpdir/modules/$isa_gate_base$isa_gate_level"
            isa_gate_attempted=$((isa_gate_attempted + 1))
            if ! mkdir -p "$isa_gate_module_dir"; then
                echo "$isa_gate_name: could not create isolated module directory for $isa_gate_base at $isa_gate_level" >&2
                isa_gate_setup_failures=$((isa_gate_setup_failures + 1))
                continue
            fi

            if [ -n "$isa_gate_source_flag" ]; then
                if "$isa_gate_compiler" -S "$isa_gate_level" "$isa_gate_source_flag" \
                    -J "$isa_gate_module_dir" "$isa_gate_source" \
                    -o "$isa_gate_out" 2>"$isa_gate_err"; then
                    isa_gate_compile_status=0
                else
                    isa_gate_compile_status=$?
                fi
            elif "$isa_gate_compiler" -S "$isa_gate_level" \
                -J "$isa_gate_module_dir" "$isa_gate_source" \
                -o "$isa_gate_out" 2>"$isa_gate_err"; then
                isa_gate_compile_status=0
            else
                isa_gate_compile_status=$?
            fi

            if [ "$isa_gate_compile_status" -ne 0 ]; then
                echo "$isa_gate_name: compilation failed for $isa_gate_base at $isa_gate_level (exit $isa_gate_compile_status)" >&2
                if [ -s "$isa_gate_err" ]; then
                    sed -n '1,20{s/^/  /;p;}' "$isa_gate_err" >&2
                fi
                isa_gate_compile_failures=$((isa_gate_compile_failures + 1))
                continue
            fi
            if [ ! -s "$isa_gate_out" ]; then
                echo "$isa_gate_name: compiler produced no assembly for $isa_gate_base at $isa_gate_level" >&2
                isa_gate_output_failures=$((isa_gate_output_failures + 1))
                continue
            fi

            isa_gate_checked=$((isa_gate_checked + 1))
            if isa_gate_validate_assembly "$isa_gate_out" >&2; then
                :
            else
                isa_gate_scan_status=$?
                if [ "$isa_gate_scan_status" -eq 1 ]; then
                    echo "$isa_gate_name: $isa_gate_hit_label in $isa_gate_base at $isa_gate_level" >&2
                    isa_gate_hits=$((isa_gate_hits + 1))
                else
                    echo "$isa_gate_name: policy checker failed for $isa_gate_base at $isa_gate_level (exit $isa_gate_scan_status)" >&2
                    isa_gate_scan_failures=$((isa_gate_scan_failures + 1))
                fi
            fi
        done
    done

    if [ "$isa_gate_diagnostics" -ne "$isa_gate_expected_diagnostics" ]; then
        echo "$isa_gate_name: expected $isa_gate_expected_diagnostics diagnostic fixtures, found $isa_gate_diagnostics" >&2
        isa_gate_setup_failures=$((isa_gate_setup_failures + 1))
    fi

    isa_gate_expected_checked=$(( (isa_gate_expected_sources - isa_gate_expected_diagnostics) * isa_gate_levels_count ))
    if [ "$isa_gate_attempted" -ne "$isa_gate_expected_checked" ]; then
        echo "$isa_gate_name: expected $isa_gate_expected_checked compiler attempts, made $isa_gate_attempted" >&2
        isa_gate_setup_failures=$((isa_gate_setup_failures + 1))
    fi
    if [ "$isa_gate_checked" -ne "$isa_gate_expected_checked" ]; then
        echo "$isa_gate_name: expected $isa_gate_expected_checked checked assemblies, got $isa_gate_checked" >&2
    fi

    if [ "$isa_gate_compile_failures" -ne 0 ] ||
        [ "$isa_gate_output_failures" -ne 0 ] ||
        [ "$isa_gate_scan_failures" -ne 0 ] ||
        [ "$isa_gate_hits" -ne 0 ] ||
        [ "$isa_gate_setup_failures" -ne 0 ]; then
        echo "$isa_gate_name: failed (compile=$isa_gate_compile_failures, empty-output=$isa_gate_output_failures, scanner=$isa_gate_scan_failures, forbidden=$isa_gate_hits, setup=$isa_gate_setup_failures)" >&2
        return 1
    fi

    echo "$isa_gate_name: clean ($isa_gate_checked assemblies, $isa_gate_diagnostics expected diagnostics excluded; $isa_gate_level_label)"
}
