! GVN must not hash-cons a PURE function call across an
! intervening store to a module variable the callee reads.
!
! Per F2018 15.7 a PURE function can reference (but not assign)
! variables accessed by host/use association — so the Fortran
! `is_pure` flag does not imply "result depends only on arguments".
! The earlier GVN pure-call policy trusted is_pure verbatim, so
! two calls to `fake_pure(5)` with a `counter = 100` assignment
! between them got collapsed to a single call at O2+, producing
! `0 0` instead of `0 600`.
module pure_mod
  implicit none
  integer :: counter = 0
contains
  pure recursive integer function fake_pure(x) result(r)
    integer, intent(in) :: x
    if (x <= 0) then
      r = counter
    else
      r = fake_pure(x - 1) + counter
    end if
  end function fake_pure
end module pure_mod

program gvn_pure_module_global
  use pure_mod
  implicit none
  integer :: a, b
  a = fake_pure(5)
  counter = 100
  b = fake_pure(5)
  print *, a, b
end program gvn_pure_module_global
! CHECK: 0 600
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
