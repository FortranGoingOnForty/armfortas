! Float DO-loop vectorization. The body has a load + invariant scalar
! broadcast and a load * invariant scalar broadcast, both producing
! 4×f32 vectors. Without the DupEl-vs-DupGen fix, isel would emit
! `dup.4s v0, sN` which the assembler rejects.
!
! CHECK: 2.5000000E0
! CHECK: 6.4000000E1
program test_do_loop_vectorize_f32
  implicit none
  integer :: i
  real(4) :: a(32), b(32), c(32)

  do i = 1, 32
    b(i) = real(i, 4)
  end do

  do i = 1, 32
    a(i) = b(i) + 1.5
    c(i) = b(i) * 2.0
  end do

  print *, a(1)
  print *, c(32)
end program test_do_loop_vectorize_f32
