! Stack-allocated fixed-size array with subscripted reads/writes.
! Used to crash the assembler with `mul x, w, x` because the
! GEP index width-extension was gated on info.allocatable, leaving
! stack arrays at i32 while the multiply landed in i64-class regs.
! Audit CRITICAL-1.
!
! CHECK: 10
! CHECK: 20
! CHECK: 30
! CHECK: 100
! CHECK: 200
! CHECK: 300
program stack_array_subscripts
  integer :: a(3)
  integer :: i

  a(1) = 10
  a(2) = 20
  a(3) = 30

  do i = 1, 3
    print *, a(i)
  end do

  ! Read-then-write through subscripts to confirm the load path works too.
  a(1) = a(1) * 10
  a(2) = a(2) * 10
  a(3) = a(3) * 10

  do i = 1, 3
    print *, a(i)
  end do
end program
