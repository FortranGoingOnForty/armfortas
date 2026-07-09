! Runtime partial-unroll remainder construction must keep branch args
! synchronized with the generated remainder header params.
!
! CHECK: ok 268 1845549155
program ice12
  implicit none
  integer :: k, j
  integer(kind=8) :: seed
  integer :: arr(8)

  seed = 123456789_8
  arr = 0
  if (nextr(2) == 0) then
    k = mod(nextr(99), 5) + 1
    do j = 1, k
      arr(j) = nextr(99)
    end do
  end if
  print *, 'ok', sum(arr), seed

contains
  integer function nextr(m) result(v)
    integer, intent(in) :: m
    seed = mod(seed * 48271_8, 2147483647_8)
    v = int(mod(seed / 17_8, int(m, kind=8)))
  end function nextr
end program ice12
