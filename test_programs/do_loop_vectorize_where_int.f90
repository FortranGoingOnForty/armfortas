! Vectorize a Fortran WHERE block over an integer counted loop.
! Same shape as the f32 fixture but with i32 arrays and `icmp gt`
! lowering through `cm gt.4s` + `bsl.16b`.
!
! a(i) = i - 16 → values [-15..16]; b(i) = i → [1..32].
! After WHERE: lanes where a > 0 (i = 17..32) get b = a; rest unchanged.
! b(1)=1, b(16)=16, b(17)=1, b(32)=16.
!
! CHECK: 1 16 1 16
program test_do_loop_vectorize_where_int
  implicit none
  integer :: i, a(32), b(32)
  do i = 1, 32
    a(i) = i - 16
    b(i) = i
  end do
  where (a > 0)
    b = a
  end where
  print *, b(1), b(16), b(17), b(32)
end program test_do_loop_vectorize_where_int
