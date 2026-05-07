! Runtime-trip partial unrolling of a sum-reduction loop. The header
! has 2 params (iv + acc); the U-way unrolled main loop threads the
! accumulator across clones; the scalar remainder loop continues the
! accumulation for trailing iterations.
!
! n = 35 + command_argument_count() (no args ⇒ n = 35). Trip = 35.
! With U=4: head_count = (35/4)*4 = 32; remainder = 3.
! sum(i*i, 1..35) = 35*36*71/6 = 14910. a(35) = 35*35 = 1225.
! CHECK: 35 14910 1225
program test_loop_partial_unroll_runtime_red
  implicit none
  integer :: a(64), s, n, i
  n = 35 + command_argument_count()
  do i = 1, n
    a(i) = i * i
  end do
  s = 0
  do i = 1, n
    s = s + a(i)
  end do
  print *, n, s, a(n)
end program test_loop_partial_unroll_runtime_red
