! PURE cannot call non-pure procedure (F2018 15.7).
! ERROR_EXPECTED: not pure, elemental, or intrinsic
program t
  implicit none
contains
  subroutine impure_sub()
    print *, "side effect"
  end subroutine
  pure subroutine bad()
    call impure_sub()
  end subroutine
end program
