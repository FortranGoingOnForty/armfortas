! l04: F2023 SPLIT(STRING, SET, POS [, BACK]) — POS is INOUT iteration
! state. Forward and backward walks over a CSV-ish line, plus the
! empty-token cases (leading/doubled/trailing separators). Held
! identical across opt levels by OPT_EQ. Uses WRITE (not PRINT) for
! the bracketed tokens: PRINT with a character format inserts spurious
! spaces around variable-length items (pre-existing, noted_items/l05).
! FLAGS: --std=f2023
! CHECK: fwd[a]
! CHECK: fwd[bb]
! CHECK: fwd[ccc]
! CHECK: bwd[ccc]
! CHECK: bwd[bb]
! CHECK: bwd[a]
! CHECK: empty []|[x]|[]|[y]|[]|
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04_split
  implicit none
  character(*), parameter :: s = "a,bb,ccc"
  character(*), parameter :: e = ",x,,y,"
  integer :: pos, istart, iend
  character(len=64) :: buf

  ! Forward walk.
  pos = 0
  do
    istart = pos + 1
    call split(s, ",", pos)
    write(*, '(3A)') 'fwd[', s(istart:pos-1), ']'
    if (pos > len(s)) exit
  end do

  ! Backward walk.
  pos = len(s) + 1
  do
    iend = pos - 1
    call split(s, ",", pos, back=.true.)
    write(*, '(3A)') 'bwd[', s(pos+1:iend), ']'
    if (pos < 1) exit
  end do

  ! Empty tokens: accumulate "[]|[x]|[]|[y]|[]|".
  buf = ''
  pos = 0
  do
    istart = pos + 1
    call split(e, ",", pos)
    buf = trim(buf) // '[' // e(istart:pos-1) // ']|'
    if (pos > len(e)) exit
  end do
  write(*, '(2A)') 'empty ', trim(buf)

  write(*, '(A)') 'ok'
end program l04_split
