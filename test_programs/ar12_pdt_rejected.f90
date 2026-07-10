! ERROR_EXPECTED: parameterized derived types (PDTs) are not supported
program ar12_pdt_rejected
  implicit none
  type :: vec
    integer :: d(3)
  end type
  type(vec(n=3)) :: v
end program ar12_pdt_rejected
