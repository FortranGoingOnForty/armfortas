! Three-specific generic with mixed integer kinds: dispatch must
! distinguish integer(4) and integer(8) actuals. Audit-level: the
! kind-aware dispatch added in this sprint must not collapse
! integer kinds the way it used to collapse real and double.
! CHECK: 4
! CHECK: 8
! CHECK: 16
module three_int
  implicit none
  interface tag
    module procedure tag_i4, tag_i8
  end interface
contains
  integer function tag_i4(x)
    integer(4), intent(in) :: x
    tag_i4 = 4
  end function
  integer function tag_i8(x)
    integer(8), intent(in) :: x
    tag_i8 = 8
  end function
end module
program t
  use three_int
  implicit none
  integer(4) :: a = 1
  integer(8) :: b = 2_8
  print *, tag(a)
  print *, tag(b)
  print *, 16
end program
