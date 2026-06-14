! l05-3: F2008/F2018 auto-reallocation of a deferred-length allocatable
! character on internal WRITE. write(s, fmt) reallocates s to the exact
! record length; len(s) tracks it. Covers grow-from-unallocated, shrink,
! re-grow, and a self-referential write (the target appears in the output
! list) which exercises the allocate-new-before-free ordering. WRITE (not
! PRINT) carries the format so the field contents are exact. Runtime-
! threaded, so opt-level invariant (OPT_EQ).
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

  write(s, '(A)') 'x'                          ! shrink -> 1
  write(*, '(A,A,A,I0)') 'b |', s, '| ', len(s)

  write(s, '(A,I0)') 'hello, world #', 100     ! re-grow -> 17
  write(*, '(A,A,A,I0)') 'c |', s, '| ', len(s)

  s = 'ab'
  write(s, '(A,A)') s, s                       ! self-referential -> 'abab'
  write(*, '(A,A,A,I0)') 'd |', s, '| ', len(s)

  print '(A)', 'ok'
end program x_l05_internal_write_realloc
