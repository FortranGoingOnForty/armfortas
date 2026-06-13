! l04a: F2023 TOKENIZE Form 2 — CALL TOKENIZE(STRING, SET, FIRST, LAST).
! FIRST/LAST are allocatable integer arrays the call allocates to the
! token count, holding 1-based start/end positions (empty token has
! LAST = FIRST-1). Held identical across opt levels by OPT_EQ.
! FLAGS: --std=f2023
! CHECK: n 3
! CHECK: t 1 1
! CHECK: t 3 4
! CHECK: t 6 8
! CHECK: en 5
! CHECK: e 1 0
! CHECK: e 2 2
! CHECK: e 4 3
! CHECK: e 5 5
! CHECK: e 7 6
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04a_tokenize_positions
  implicit none
  character(*), parameter :: s = "a,bb,ccc"
  character(*), parameter :: e = ",x,,y,"
  integer, allocatable :: first(:), last(:)
  integer :: i, n

  call tokenize(s, ",", first, last)
  n = size(first)
  write(*, '(A,1X,I0)') 'n', n
  do i = 1, n
    write(*, '(A,1X,I0,1X,I0)') 't', first(i), last(i)
  end do

  call tokenize(e, ",", first, last)
  n = size(first)
  write(*, '(A,1X,I0)') 'en', n
  do i = 1, n
    write(*, '(A,1X,I0,1X,I0)') 'e', first(i), last(i)
  end do

  write(*, '(A)') 'ok'
end program l04a_tokenize_positions
