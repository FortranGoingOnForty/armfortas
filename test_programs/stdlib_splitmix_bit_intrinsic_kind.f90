! CHECK: -4394453597509714643
! CHECK: ok
! REPRO_CHECK: run
module stdlib_splitmix_bit_intrinsic_kind_mod
  use iso_fortran_env, only: int8, int64
  implicit none
  private
  public :: splitmix64
contains
  function dist8(n) result(res)
    integer(int8), intent(in) :: n
    integer(int8) :: res
    integer :: k
    k = 64 - bit_size(n)
    res = shiftr(source64(), k)
  end function

  function source64() result(res)
    integer(int64) :: res
    res = 123456789_int64
  end function

  function splitmix64() result(res)
    integer(int64) :: res
    integer(int64) :: int02
    int02 = -4658895280553007687_int64
    res = source64()
    res = ieor(res, shiftr(res, 30)) * int02
  end function
end module

program stdlib_splitmix_bit_intrinsic_kind
  use iso_fortran_env, only: int64
  use stdlib_splitmix_bit_intrinsic_kind_mod, only: splitmix64
  implicit none
  integer(int64) :: got
  got = splitmix64()
  if (got /= -4394453597509714643_int64) error stop 1
  print *, got
  print *, 'ok'
end program
