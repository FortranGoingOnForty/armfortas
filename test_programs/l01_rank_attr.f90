! l01: RANK(n) attribute (F2023 8.5.17) on an allocatable and on a
! dummy argument; desugars to deferred/assumed-shape of rank n.
! FLAGS: --std=f2023
! CHECK: 2 3
! CHECK: 21
! CHECK: 4
! CHECK: 5
program l01_rank_attr
  implicit none
  integer, rank(2), allocatable :: a
  integer :: i, j
  allocate(a(2, 3))
  do i = 1, 2
    do j = 1, 3
      a(i, j) = (i - 1) * 3 + j
    end do
  end do
  print *, shape(a)
  print *, sum(a)
  call takes_rank1(a(2, :))
contains
  subroutine takes_rank1(v)
    integer, rank(1) :: v
    print *, size(v) + 1
    print *, v(2)
  end subroutine takes_rank1
end program l01_rank_attr
