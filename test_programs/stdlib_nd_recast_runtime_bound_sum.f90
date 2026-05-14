! CHECK: ok
! IR_CHECK: call @afs_array_sum_real8(
! REPRO_CHECK: run
module stdlib_nd_recast_runtime_bound_sum_mod
contains
  function outer(x) result(s)
    real, intent(in) :: x(:,:,:)
    real :: s

    s = inner(x, size(x))
  contains
    real function inner(b, n)
      integer, intent(in) :: n
      real, intent(in) :: b(n)

      inner = sum(b)
    end function
  end function
end module

program stdlib_nd_recast_runtime_bound_sum
  use stdlib_nd_recast_runtime_bound_sum_mod
  implicit none
  real :: x(2,2,2)

  x = 1.0
  if (abs(outer(x) - 8.0) > 1.0e-6) error stop 1

  print *, 'ok'
end program
