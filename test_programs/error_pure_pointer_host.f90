! PURE cannot pointer-assign a host-associated pointer (F2018 15.7).
! ERROR_EXPECTED: host or use association
program t
  implicit none
  integer, pointer :: ptr
  integer, target :: tgt = 42
contains
  pure subroutine bad()
    ptr => tgt
  end subroutine
end program
