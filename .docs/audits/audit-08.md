# Audit 08 — runtime I/O and platform behavior

Reviewed implementation commit `23857aa48f3bc0160303842488e8578acb487fb1` on x86_64 Linux. I inspected the runtime and the compiler lowering that calls it, without reading any other audit report. Validation used focused programs compiled from `/dev/stdin` with the existing `target/debug/armfortas`; no full workspace suite was run. Where useful, the same input was compared with the local gfortran. Temporary executables and data stayed under `/tmp`.

Severity used below:

- **High** — silent data loss/corruption, memory overwrite, nontermination, or target-wide wrong numerical behavior.
- **Medium** — observable standards/API mismatch with narrower impact, or an accumulating resource leak.

All confirmed findings below have **high confidence**: they were reproduced at the compiler boundary on this host, except the FreeBSD ABI finding, which is established directly by armfortas source and FreeBSD's own target headers.

## Confirmed discrepancies

### A08-01 — High — `STATUS='SCRATCH'` plus `FILE=` deletes the named file

- **Source:** `runtime/src/io_system.rs:623-639`, `676-688`, `741-755`, `847-872`, and `952-966`.
- **Cause:** `afs_open` recognizes scratch status but rejects neither a simultaneous `FILE=` nor an existing named file. It uses the caller's name, marks the unit as scratch, and `CLOSE` later unlinks that name.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: ios
    logical :: ex
    open(10,file='/tmp/a08-victim',status='replace',action='write')
    write(10,'(A)') 'keep'
    close(10)
    ios=77
    open(11,file='/tmp/a08-victim',status='scratch',iostat=ios)
    if (ios == 0) close(11)
    inquire(file='/tmp/a08-victim',exist=ex)
    write(*,'(I0,1X,L1)') ios,ex
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `0 F`; the pre-existing named file is deleted.
- **Intended:** The prohibited `FILE=`/`STATUS='SCRATCH'` combination must fail (`IOSTAT` nonzero) and leave the file in place. The comparison compiler produced `5002 T`.
- **Consequence:** A malformed or dynamically constructed `OPEN` can destroy user data rather than reporting an I/O error.
- **Confidence:** High.

### A08-02 — High — `INQUIRE(RECL=integer(4))` writes eight bytes and corrupts adjacent storage

- **Source:** lowering passes the destination address directly at `src/ir/lower/stmt.rs:8333` and `8403-8439`; the runtime declares `recl_out: *mut i64` and writes through it at `runtime/src/io_system.rs:3649-3665` and `3901-3904`. `SIZE=` and `POS=` correctly use i64 temporaries plus typed storeback at `src/ir/lower/stmt.rs:8334-8355`, highlighting the missing conversion for `RECL=`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O0 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    type pair
      integer(4) :: recl
      integer(4) :: guard
    end type
    type(pair) :: q
    integer :: u
    q%recl=0; q%guard=123456
    open(newunit=u,status='scratch',access='stream',form='formatted')
    inquire(unit=u,recl=q%recl)
    write(*,'(2(I0,1X))') q%recl,q%guard
    close(u)
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `-1 -1`; the upper four bytes of the i64 store overwrite `q%guard`.
- **Intended:** `RECL` receives a processor-dependent value in its declared kind and `q%guard` remains `123456` (the comparison compiler printed `-2 123456`).
- **Consequence:** Ordinary default-integer `INQUIRE` can corrupt stack, component, or heap data adjacent to the result.
- **Confidence:** High.

### A08-03 — High — logical input items are silently omitted

- **Source:** `lower_list_read_items` keeps lowering after a helper returns false at `src/ir/lower/core.rs:31801-31838`; `lower_read_into_addr` has numeric and complex arms but no `IrType::Bool` arm and falls through at `src/ir/lower/core.rs:31988-32400`. There is no logical-read runtime entry point.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    logical :: l
    integer :: ios
    character(1) :: src
    src='T'; l=.false.; ios=77
    read(src,*,iostat=ios) l
    write(*,'(L1,1X,I0)') l,ios
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `F 77`; neither the value nor `IOSTAT` is touched.
- **Intended:** `T 0`.
- **Consequence:** Logical configuration and data fields are ignored while execution continues with stale values and stale status.
- **Confidence:** High.

### A08-04 — High — a later successful READ item erases an earlier error, and `IOMSG` is unwired

- **Source:** all items are emitted unconditionally at `src/ir/lower/core.rs:31824-31838`. Each runtime helper writes zero on its own success, for example `runtime/src/io_system.rs:3165-3181` and `3239-3255`. Lowering collects `IOMSG` at `src/ir/lower/stmt.rs:8129-8146`, but internal reads never receive it; external list begin merely blanks it (`runtime/src/io_system.rs:1846-1857`), read end ignores it (`1900-1909`), and formatted readers have no message argument.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: a,b,ios
    character(6) :: src
    character(32) :: msg
    src='bad 42'; a=7; b=8; ios=77; msg='sentinel'
    read(src,*,iostat=ios,iomsg=msg) a,b
    write(*,'(3(I0,1X),A)') ios,a,b,trim(msg)
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `0 7 42 sentinel`.
- **Intended:** The statement stops at item 1, returns nonzero status, supplies an explanatory message, and does not consume/assign item 2. The comparison result was `5010 7 8 Bad integer for item 1 ...`.
- **Consequence:** Callers can accept a partially updated record as successful, with no usable diagnostic.
- **Confidence:** High.

### A08-05 — High — list-directed null fields are collapsed instead of preserving their item position

- **Source:** external tokenization splits on commas/whitespace and filters empty pieces at `runtime/src/io_system.rs:360-383`; internal tokenization skips every comma at `3117-3153`. Neither path represents a null value.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: a,b,ios
    character(3) :: src
    src=',42'; a=7; b=8; ios=77
    read(src,*,iostat=ios) a,b
    write(*,'(3(I0,1X))') ios,a,b
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `-1 42 8`; `42` is shifted into `a`, then the second item reports end-of-file.
- **Intended:** `0 7 42`; a null list-directed field leaves the corresponding variable unchanged.
- **Consequence:** CSV-like and sparse Fortran records silently shift values into the wrong variables.
- **Confidence:** High.

### A08-06 — High — formatted real input discards descriptor state, including the implied decimal

- **Source:** fields are sliced by width at `runtime/src/io_system.rs:4739-4766`, but real readers pattern-match descriptors as `{ .. }` and only Rust-parse their text at `5393-5416` and `5584-5605`. The `d` in `Fw.d`, scale factor, `BN`/`BZ`, and `DC`/`DP` state never reaches numeric conversion.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    real(8) :: x
    integer :: ios
    character(5) :: src
    src='00123'; x=-1; ios=77
    read(src,'(F5.2)',iostat=ios) x
    write(*,'(I0,1X,F6.2)') ios,x
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `0 123.00`.
- **Intended:** `0   1.23`; absent an explicit decimal point, `F5.2` supplies two fractional digits.
- **Consequence:** Fixed-column scientific and financial inputs can be wrong by powers of ten while reporting success.
- **Confidence:** High.

### A08-07 — High — list-directed WRITE discards permission and write errors

- **Source:** item writers discard `write_str`/`write_bytes` results throughout `runtime/src/io_system.rs:1009-1173`; newline and flush errors are discarded at `1178-1207`, and list-write end ignores the formatted-unit flush at `1303-1350`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: ios
    character(40) :: msg
    open(10,file='/tmp/a08-ro',status='replace',action='write')
    write(10,*) 1
    close(10)
    open(10,file='/tmp/a08-ro',status='old',action='read')
    ios=77; msg='sentinel'
    write(10,*,iostat=ios,iomsg=msg) 42
    write(*,'(I0,1X,A)') ios,trim(msg)
    close(10,status='delete')
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `0` with a blank message, although no record was written.
- **Intended:** Nonzero status and an explanatory message; the comparison runtime returned `5007 Cannot write to file opened for READ`.
- **Consequence:** Applications believe output was persisted when it was rejected.
- **Confidence:** High.

### A08-08 — High — scalar internal formatted WRITE silently truncates and reports success

- **Source:** `afs_fmt_end` checks for extra records but never compares the record length with the scalar buffer at `runtime/src/io_system.rs:4556-4591`; `write_to_buffer` truncates to available space at `3340-3358` and the statement then stores `IOSTAT=0` at `4683-4697`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    character(3) :: buf
    integer :: ios
    buf='???'; ios=77
    write(buf,'(A)',iostat=ios) 'abcdef'
    write(*,'(I0,1X,A)') ios,buf
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `0 abc`.
- **Intended:** End-of-record/record-overflow status, not silent success. The comparison runtime returned `-2` and did not perform the truncated assignment.
- **Consequence:** Internal serialization loses data without giving the caller a chance to resize or recover.
- **Confidence:** High.

### A08-09 — High — external NAMELIST READ loops forever at EOF

- **Source:** `afs_read_namelist` holds the process-global I/O mutex from `runtime/src/io_system.rs:2621`; `read_line()` represents EOF as `Ok("")`, but both loops at `2625-2649` and `2632-2637` only terminate on a matching group or `Err`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: x,ios
    namelist /g/ x
    open(10,file='/tmp/a08-nml',status='replace',action='write')
    close(10)
    open(10,file='/tmp/a08-nml',status='old',action='read')
    read(10,nml=g,iostat=ios)
    write(*,'(I0)') ios
  end program
  F90
  timeout 2s /tmp/a08; echo "exit=$?"
  ```

- **Actual:** No output; `timeout` reports `exit=124`.
- **Intended:** Prompt `IOSTAT_END` (the comparison runtime printed `-1` and exited normally).
- **Consequence:** A missing group or empty file hangs the program and, because the global mutex remains held, blocks every other runtime I/O unit too.
- **Confidence:** High.

### A08-10 — High — `POS=` seeks the file handle but leaves stale list/parser state

- **Source:** `afs_seek_stream` only seeks at `runtime/src/io_system.rs:2434-2475`; it does not clear `read_tokens`, `formatted_read_record`, its cursor, or pending record state. Lowering calls it before the transfer at `src/ir/lower/stmt.rs:1492-1509`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: a,b,ios
    open(10,file='/tmp/a08-pos',status='replace',access='stream', &
         form='formatted',action='readwrite')
    write(10,'(A)') '1 2'
    a=9; b=9
    read(unit=10,fmt=*,pos=1,iostat=ios) a
    read(unit=10,fmt=*,pos=1,iostat=ios) b
    write(*,'(3(I0,1X))') ios,a,b
    close(10,status='delete')
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `0 1 2`; the second read pops the token cached before the seek.
- **Intended:** `0 1 1`; each `POS=1` transfer starts at file storage unit 1.
- **Consequence:** Random-access parsing returns data from the old position while reporting a successful seek/read.
- **Confidence:** High.

### A08-11 — High — `BACKSPACE` and `ENDFILE` statements are dropped during lowering

- **Source:** parser/AST support exists, but `lower_stmt` has arms only for `FLUSH` and `REWIND` at `src/ir/lower/stmt.rs:8455-8483`; `BACKSPACE` and `ENDFILE` reach the no-op catch-all at `9354`. Runtime entry points at `runtime/src/io_system.rs:3366-3437` consequently have no generated-code callers.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    character(8) :: s
    integer :: ib,ie
    open(10,file='/tmp/a08-bs',status='replace',action='write')
    write(10,'(A)') 'first'; write(10,'(A)') 'second'; close(10)
    open(10,file='/tmp/a08-bs',status='old',action='readwrite')
    read(10,'(A)') s
    ib=77; backspace(10,iostat=ib)
    read(10,'(A)') s
    ie=88; endfile(10,iostat=ie)
    write(*,'(I0,1X,A,1X,I0)') ib,trim(s),ie
    close(10,status='delete')
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `77 second 88`; both preset status values survive, and BACKSPACE did not reposition.
- **Intended:** `0 first 0`; the comparison runtime performed both positioning statements and returned success.
- **Consequence:** Record-rewrite and file-position algorithms continue at the wrong record; even status variables reveal no operation occurred.
- **Confidence:** High.

### A08-12 — Medium — output FORMAT reversion restarts the whole format, not the rightmost nested group

- **Source:** `FormatEngine::format_values_reverting_bytes_checked` repeatedly applies the full top-level descriptor slice at `runtime/src/format.rs:723-749`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    write(*,'("P",2(I0,1X))') 1,2,3,4
  end program
  F90
  /tmp/a08
  ```

- **Actual:** two records, `P1 2` and `P3 4`.
- **Intended:** `P1 2` and `3 4`; reversion resumes at the rightmost nested group and does not repeat the outer literal.
- **Consequence:** Headers, prefixes, positioning, and control descriptors are spuriously repeated in multi-record formatted output.
- **Confidence:** High.

### A08-13 — Medium — `EN` formatting uses the wrong engineering exponent below one

- **Source:** the `RealEN` output arm is at `runtime/src/format.rs:974-992`; `to_engineering` computes `(floor(log10(v)) as i32) / 3 * 3` at `1599-1605`. Rust integer division truncates negative values toward zero rather than toward negative infinity.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    write(*,'(EN14.3)') 0.01d0
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `     0.010E+00`.
- **Intended:** `    10.000E-03`, with an exponent divisible by three and a mantissa in the engineering range.
- **Consequence:** Output violates the requested descriptor and breaks consumers that rely on engineering exponent grouping.
- **Confidence:** High.

### A08-14 — High — `CPU_TIME` has the wrong ABI and scale on advertised x86_64-FreeBSD

- **Source:** `runtime/src/system.rs:62-68` declares `clock() -> i64` and hardcodes `CLOCKS_PER_SEC=1_000_000` as a macOS value for every target.
- **Target contract:** FreeBSD's [`<time.h>`](https://raw.githubusercontent.com/freebsd/freebsd-src/main/include/time.h) defines `CLOCKS_PER_SEC` as 128, and its [x86 type header](https://raw.githubusercontent.com/freebsd/freebsd-src/main/sys/x86/include/_types.h) defines LP64 `__clock_t` as signed 32-bit.
- **Reproducer (run on x86_64-FreeBSD):**

  ```sh
  printf '#include <stdio.h>\n#include <time.h>\nint main(void){printf("%zu %.0f\\n",sizeof(clock_t),(double)CLOCKS_PER_SEC);}\n' |
    cc -x c -o /tmp/a08-clock - && /tmp/a08-clock
  ```

- **Actual:** The native probe prints `4 128`, while armfortas reads an i64 return and divides it by 1,000,000. One CPU second (128 ticks) is therefore reported as `0.000128` seconds, 7812.5 times too small; error/wrap sign handling also uses the wrong return ABI.
- **Intended:** Use the target's `clock_t` ABI and `CLOCKS_PER_SEC`, yielding approximately `1.0` second for 128 ticks.
- **Consequence:** Every `CPU_TIME` result is badly wrong on a documented target.
- **Confidence:** High, based on the local implementation and primary FreeBSD headers.

### A08-15 — High — Unix argv and environment bytes are lossy-decoded as UTF-8

- **Source:** `program_args` calls `CStr::to_string_lossy` at `runtime/src/system.rs:209-224`. Environment names are lossy-decoded and Unicode-trimmed, and values are fetched with Unicode-only `std::env::var`, at `325-371`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    character(4) :: a,e
    integer :: an,as,en,es
    a='????'; e='????'
    call get_command_argument(1,value=a,length=an,status=as)
    call get_environment_variable('X',value=e,length=en,status=es)
    write(*,'(3(I0,1X))') an,as,ichar(a(1:1))
    write(*,'(3(I0,1X))') en,es,ichar(e(1:1))
  end program
  F90
  python3 -c 'import os; e=os.environb.copy(); e[b"X"]=b"\xff"; os.execve(b"/tmp/a08",[b"/tmp/a08",b"\xff"],e)'
  ```

- **Actual:** Argument line `3 0 239` (`FF` became UTF-8 `EF BF BD`); environment line `0 1 63` (the existing non-UTF-8 value is reported missing and the destination remains `?`).
- **Intended:** Both interfaces preserve the one Unix byte: `1 0 255`.
- **Consequence:** File names passed as arguments, locale data, and arbitrary byte-valued environment settings change length/content or disappear. This contrasts with the OPEN path, which correctly uses Unix `OsStringExt` at `runtime/src/io_system.rs:38-57`.
- **Confidence:** High.

### A08-16 — Medium — GET command/environment intrinsics report success after truncation

- **Source:** `GET_COMMAND_ARGUMENT` copies `min(actual,value_len)` then always stores zero status at `runtime/src/system.rs:257-280`; `GET_COMMAND` does the same at `285-312`; `GET_ENVIRONMENT_VARIABLE` does so at `337-359`. Their missing/out-of-range paths also leave `VALUE` unchanged instead of blank-filling it (`243-254`, `360-371`).
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    character(2) :: v
    integer :: n,s
    v='??'; n=77; s=77
    call get_command_argument(1,value=v,length=n,status=s)
    write(*,'(I0,1X,I0,1X,A)') n,s,v
  end program
  F90
  /tmp/a08 abcdef
  ```

- **Actual:** `6 0 ab`.
- **Intended:** `6 -1 ab`; `STATUS=-1` tells the caller the returned character value was truncated.
- **Consequence:** Callers cannot distinguish a complete value from a truncated command, argument, or environment value.
- **Confidence:** High.

### A08-17 — High — `IEEE_SCALB` overflows an intermediate when the mathematical result is finite

- **Source:** despite its comment, `runtime/src/ieee.rs:282-290` implements `x * 2.powi(i)` rather than a scaling operation that avoids an intermediate power.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O0 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    use, intrinsic :: ieee_arithmetic
    real(8) :: x,y
    integer :: n
    x=tiny(1.0d0); n=1024
    y=ieee_scalb(x,n)
    write(*,'(L1,1X,F5.1)') ieee_is_finite(y),y
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `F   Inf`, because `2^1024` overflows first.
- **Intended:** `T   4.0`; scaling `2^-1022` by `2^1024` is exactly four.
- **Consequence:** Numerically stable exponent manipulation produces spurious infinities/zeros at ordinary representable results.
- **Confidence:** High.

### A08-18 — Medium — `IEEE_VALUE(..., IEEE_SIGNALING_NAN)` constructs a quiet NaN

- **Source:** both signaling- and quiet-NaN class tags select the same quiet bit pattern at `runtime/src/ieee.rs:186-204`.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O0 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    use, intrinsic :: ieee_arithmetic
    real(8) :: x
    x=ieee_value(x,ieee_signaling_nan)
    write(*,'(2(L1,1X))') ieee_class(x)==ieee_signaling_nan, &
                          ieee_class(x)==ieee_quiet_nan
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `F T`.
- **Intended:** `T F`, as also produced by the comparison compiler.
- **Consequence:** Code cannot construct/test the requested IEEE class, and signaling-NaN behavior and exception tests become meaningless.
- **Confidence:** High.

### A08-19 — Medium — the explicit ROUND argument to F2023 `IEEE_RINT` is ignored

- **Source:** lowering accepts the intrinsic but forwards only argument zero at `src/ir/lower/intrinsic.rs:1577-1588`; runtime entry points accept only `x` at `runtime/src/ieee.rs:270-280`.
- **Reproducer:**

  ```sh
  target/debug/armfortas --std=f2023 -O0 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    use, intrinsic :: ieee_arithmetic
    real(8) :: x
    call ieee_set_rounding_mode(ieee_down)
    x=ieee_rint(1.1d0,ieee_up)
    write(*,'(F3.0)') x
    call ieee_set_rounding_mode(ieee_nearest)
  end program
  F90
  /tmp/a08
  ```

- **Actual:** ` 1.`; the explicit `IEEE_UP` is discarded and the ambient downward mode wins on this x86 build.
- **Intended:** ` 2.` from the rounding mode passed to the intrinsic.
- **Consequence:** Results depend on ambient thread FP state even when the program supplies an explicit mode.
- **Confidence:** High.

### A08-20 — High — TOKENIZE uses FIRST's integer kind for both FIRST and LAST

- **Source:** lowering derives only the third argument's element kind at `src/ir/lower/intrinsic_sub.rs:421-435`; the runtime has a single `int_kind` parameter and allocates/writes both arrays with it at `runtime/src/tokenize.rs:69-95`. Fortran does not require FIRST and LAST to have the same kind.
- **Reproducer:**

  ```sh
  target/debug/armfortas --std=f2023 -O0 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer(4), allocatable :: first(:)
    integer(8), allocatable :: last(:)
    call tokenize('a,b',',',first,last)
    write(*,'(4(I0,1X))') first(1),first(2),last(1),last(2)
  end program
  F90
  /tmp/a08
  ```

- **Actual:** `1 3 12884901889 0`; LAST is allocated as 4-byte elements and later consumed as 8-byte elements.
- **Intended:** `1 3 1 3`.
- **Consequence:** Mixed-kind standard calls corrupt descriptor interpretation and can read beyond the allocated payload.
- **Confidence:** High.

### A08-21 — Medium — asynchronous EXECUTE_COMMAND_LINE accumulates zombie children

- **Source:** the `WAIT=.FALSE.` path calls `Command::spawn` and immediately drops `Child` at `runtime/src/system.rs:419-425`; no SIGCHLD handler, waiter thread, or later reap path exists.
- **Reproducer:**

  ```sh
  target/debug/armfortas -O2 /dev/stdin -o /tmp/a08 <<'F90'
  program p
    integer :: i
    do i=1,5
      call execute_command_line('exit 0',wait=.false.)
    end do
    call execute_command_line('sleep 2')
  end program
  F90
  /tmp/a08 & p=$!
  sleep 0.5
  ps -o stat=,ppid=,pid=,comm= --ppid "$p"
  wait "$p"
  ```

- **Actual:** five `Z ... sh <defunct>` children remain until the Fortran parent exits.
- **Intended:** Completed asynchronous commands are reaped by the runtime rather than retained permanently as zombies.
- **Consequence:** A long-running program can exhaust its process table after enough asynchronous commands.
- **Confidence:** High.

## Unconfirmed concerns and hardening observations

These were not promoted to confirmed discrepancies because their standard-level consequence or target behavior was not independently demonstrated in this review.

- **Scratch-path race:** generated names are predictable PID/unit/sequence values and are opened with `create`, not `create_new` (`runtime/src/io_system.rs:64-75`, `741-755`). A stale path or symlink can redirect scratch I/O before close unlinks the path.
- **Unformatted record trust:** sequential-unformatted input trusts a u32 length for immediate allocation, accepts short data/trailer reads, and never verifies the trailing marker (`runtime/src/io_system.rs:1870-1893`). Crafted files can request about 4 GiB or be accepted with zero-filled missing bytes.
- **Fatal error finalization:** several READ helpers call `process::exit` while holding `io_state`; `afs_io_finalize` uses `try_lock` and silently skips flushing and scratch cleanup when the mutex is held (`runtime/src/io_system.rs:1356-1832`, `3975-3995`). Whether error termination must preserve each pending record was not resolved here, but this defeats the runtime's own atexit cleanup intent.
- **Other positioning controls:** `FLUSH` and `REWIND` lowering always passes a null status pointer and ignores `IOSTAT`/`IOMSG`/`ERR` controls (`src/ir/lower/stmt.rs:8455-8480`). Runtime REWIND clears list tokens but not cached formatted-record state (`runtime/src/io_system.rs:3938-3961`).
- **EOR branching:** READ lowering recognizes `END=` and `ERR=` but not `EOR=` (`src/ir/lower/stmt.rs:8048-8075`); generic status branching treats `IOSTAT_EOR=-2` as an ordinary error (`src/ir/lower/core.rs:31938-31980`).
- **Multiple units for one file:** OPEN checks unit-number reuse but not whether the same path is already connected on another unit (`runtime/src/io_system.rs:667-675`, `799-874`). Independent buffers can overwrite each other, and `INQUIRE(FILE=)` selects an arbitrary HashMap match.
- **Close/reopen errors:** CLOSE and unit replacement discard flush errors (`runtime/src/io_system.rs:799-803`, `952-971`). Reopening a scratch unit also drops the old connection without deleting its scratch file.
- **SYSTEM_CLOCK source:** the implementation claims monotonic behavior but uses `SystemTime`/`CLOCK_REALTIME` (`runtime/src/system.rs:18-36`), which can move backward under clock adjustment.
- **RNG state:** RANDOM state is thread-local despite being described as shared (`runtime/src/system.rs:439-446`). A seed set on one OS thread does not control calls scheduled on another.
- **x86 IEEE environment:** save/restore covers MXCSR only, not x87 control/status (`runtime/src/ieee.rs:650-713`). Generated SSE arithmetic is likely unaffected, but foreign or x87 library interactions need native tests.
- **IEEE flag behavior:** `IEEE_LOGB(0)` and NaN/min/max helpers synthesize results with Rust bit/arithmetic operations; required divide-by-zero/invalid flag behavior was not checked.
- **Degree/half-revolution math:** no high-confidence defect was confirmed in `runtime/src/math.rs`. Huge-argument reduction, signed pole results, dynamic rounding, and per-libm behavior remain cross-platform differential gaps.
- **Direct access:** OPEN rejects every `ACCESS='DIRECT'` at `runtime/src/io_system.rs:641-652`, leaving the implemented direct-read/write helpers unreachable from conforming generated programs. This is an explicit feature gap rather than a hidden behavior mismatch.
- **Platform delivery:** x86_64-musl is advertised by the CLI, but native linking deliberately errors as future work at `src/driver/elf_crt.rs:47-53`; this review therefore could not execute the runtime on musl. FreeBSD and macOS behavior beyond the inspected ABIs likewise needs native runs.

## Maintainability and performance observations

- A single process-global mutex guards all units (`runtime/src/io_system.rs:33-35`) and is held across blocking reads and writes. Thus blocked input on one unit serializes unrelated units. List-directed begin, each item, and end acquire it separately, so the mutex gives call-level exclusion rather than statement-level atomicity; concurrent statements can interleave or overwrite `pending_record`/`pending_read` state.
- Whole-array list output is lowered to one runtime call per element (`src/ir/lower/core.rs:35916-36019`). Each numeric helper locks the global mutex and allocates a formatting `String` (`runtime/src/io_system.rs:999-1101`), producing N locks, allocations, and writes.
- Formatted READ clones the cached full record for each item (`runtime/src/io_system.rs:4978-4997`), reparses the FORMAT and scans from its start (`4907-4918`), and allocates fields even while skipping earlier descriptors (`4739-4819`). Long records therefore approach repeated-prefix/O(n²) work.
- `read_buffer_take` allocates a new `Vec` for every scalar unformatted item (`runtime/src/io_system.rs:257-264`). The record is already owned; a checked slice/copy would avoid per-item allocation.
- Formatted output's thread-local context stack (`runtime/src/io_system.rs:4038-4063`) is a good reentrancy improvement, but the per-thread FORMAT cache clears all 128 entries at once (`4065-4078`), which can thrash workloads using more than 128 dynamic formats.
- `program_args()` rebuilds and allocates a `Vec<String>` on every command-argument query (`runtime/src/system.rs:209-224`), making repeated indexed retrieval quadratic in total argument bytes.

## Test gaps

- No native runtime matrix here for glibc versus musl versus FreeBSD versus macOS, particularly `CPU_TIME`, `DATE_AND_TIME`, fenv, non-UTF-8 argv/env, and libm boundary behavior.
- No concurrency/reentrancy tests for blocking one unit while using another, same-unit statement interleaving, finalization contention, or thread-local RNG reproducibility.
- No failure-injection tests for short writes, flush/close errors, full filesystems, malformed/truncated unformatted records, or a fatal READ while formatted output remains buffered.
- No focused tests for `FILE=` plus scratch status, same-file/two-unit connections, nondefault integer kinds in I/O control/result specifiers, stale parser state after `POS=`/REWIND, or `BACKSPACE`/`ENDFILE` code generation.
- List-directed input coverage needs nulls, repeat counts, slash termination, quoted strings containing separators, blank records, complex/logical items, and first-item failure followed by valid later fields.
- FORMAT input tests need `d`-implied decimals, kP, BN/BZ, DC/DP, nested reversion, and persistent control state. FORMAT output differentials need negative-exponent EN, EX, `Iw.0`, `Gw.d`, nested colon/reversion, and exponent widths.
- IOSTAT/IOMSG/ERR/END/EOR need statement-wide tests for every input/output and positioning statement, including a guarantee that later items cannot erase the first error.
- IEEE tests need optional ROUND, signaling versus quiet NaN bit classes, finite-result extreme SCALB, sticky exception flags, signed zeros, subnormals, and both x86 and ARM fenv paths.
- TOKENIZE needs independent FIRST/LAST kinds, zero-length tokens, separator output, reused allocatables, and non-default character-kind rejection or implementation.
