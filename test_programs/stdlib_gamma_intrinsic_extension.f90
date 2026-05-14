! CHECK: ok
! IR_CHECK: call @tgammaf
! REPRO_CHECK: run
module gamma_like
  implicit none
  private
  public :: gamma

  interface gamma
    module procedure gamma_iint32
    module procedure gamma_csp
  end interface
contains
  elemental real function gamma_iint32(z) result(res)
    integer, intent(in) :: z

    res = real(z)
  end function

  impure elemental complex function gamma_csp(z) result(res)
    complex, intent(in) :: z

    res = cmplx(gamma(z%re), kind=4)
  end function
end module

program stdlib_gamma_intrinsic_extension
  use gamma_like, only: gamma
  implicit none

  complex :: got

  got = gamma((2.0, 0.0))

  print *, 'ok'
end program
