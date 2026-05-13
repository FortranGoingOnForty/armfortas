! CHECK: ok
! IR_CHECK: elemental_optional_arg_absent
! REPRO_CHECK: run
module m
  use iso_fortran_env, only: real32
  implicit none

  interface optval
    module procedure optval_rsp
  end interface
contains
  pure elemental function optval_rsp(x, default) result(y)
    real(real32), intent(in), optional :: x
    real(real32), intent(in) :: default
    real(real32) :: y

    if (present(x)) then
      y = x
    else
      y = default
    end if
  end function optval_rsp

  function foo_sp_arr(x) result(z)
    real(real32), dimension(2), intent(in), optional :: x
    real(real32), dimension(2) :: z

    z = optval(x, [2.0_real32, -2.0_real32])
  end function foo_sp_arr
end module m

program p
  use iso_fortran_env, only: real32
  use m, only: foo_sp_arr
  implicit none

  real(real32) :: a(2)

  a = foo_sp_arr([1.0_real32, -1.0_real32])
  if (any(a /= [1.0_real32, -1.0_real32])) error stop 1

  a = foo_sp_arr()
  if (any(a /= [2.0_real32, -2.0_real32])) error stop 2

  print *, 'ok'
end program p
