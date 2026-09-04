! Audit C12 / l05-3: F2023 §12.4 auto-reallocation of a deferred-length
! FLAGS: --std=f2023
! allocatable character on internal WRITE. When the internal file is an
! allocatable deferred-length character scalar, the record is assigned by
! intrinsic assignment, reallocating the variable to length equal to the
! number of characters written — it is NOT a fixed internal file that pads or
! truncates (that is the "otherwise" case in §12.4, for non-deferred units).
! write(s, fmt) reallocates s to the exact record length; len(s) tracks it.
! Covers grow-from-unallocated, shrink, re-grow, and a self-referential write
! (the target appears in the output list), which exercises the collect-then-
! store ordering. WRITE (not PRINT) carries the format so field contents are
! exact. Runtime-threaded, so opt-level invariant (OPT_EQ).
!
! History: commit 426a29d regressed this to fixed-length truncation and
! rewrote these CHECK lines to match; restored to the standard-conforming
! realloc (audit finding C12).
! CHECK: a |val=42!| 7
! CHECK: b |x| 1
! CHECK: c |hello, world #100| 17
! CHECK: d |abab| 4
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x_l05_internal_write_realloc
  implicit none
  character(:), allocatable :: s

  write(s, '(A,I0,A)') 'val=', 42, '!'        ! grow from unallocated -> 7
  write(*, '(A,A,A,I0)') 'a |', s, '| ', len(s)

  write(s, '(A)') 'x'                          ! reallocate -> 1
  write(*, '(A,A,A,I0)') 'b |', s, '| ', len(s)

  write(s, '(A,I0)') 'hello, world #', 100     ! reallocate -> 17
  write(*, '(A,A,A,I0)') 'c |', s, '| ', len(s)

  s = 'ab'                                     ! assignment reallocates -> 2
  write(s, '(A,A)') s, s                       ! self-referential -> 'abab', 4
  write(*, '(A,A,A,I0)') 'd |', s, '| ', len(s)

  print '(A)', 'ok'
end program x_l05_internal_write_realloc
