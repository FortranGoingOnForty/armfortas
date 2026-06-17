! l05-3: formatted internal WRITE to a deferred-length allocatable character.
! An unallocated target is allocated to the first record length; an already
! allocated target behaves like a fixed internal file, preserving length while
! padding or truncating the formatted record. Assignment to the same variable
! still uses normal allocatable character reallocation semantics.
! CHECK: a |val=42!| 7
! CHECK: b |x      | 7
! CHECK: c |hello, | 7
! CHECK: d |ab| 2
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x_l05_internal_write_realloc
  implicit none
  character(:), allocatable :: s

  write(s, '(A,I0,A)') 'val=', 42, '!'        ! grow from unallocated -> 7
  write(*, '(A,A,A,I0)') 'a |', s, '| ', len(s)

  write(s, '(A)') 'x'                          ! fixed internal file -> 7
  write(*, '(A,A,A,I0)') 'b |', s, '| ', len(s)

  write(s, '(A,I0)') 'hello, world #', 100     ! truncate to current len -> 7
  write(*, '(A,A,A,I0)') 'c |', s, '| ', len(s)

  s = 'ab'                                     ! assignment reallocates -> 2
  write(s, '(A,A)') s, s                       ! truncate to current len -> 'ab'
  write(*, '(A,A,A,I0)') 'd |', s, '| ', len(s)

  print '(A)', 'ok'
end program x_l05_internal_write_realloc
