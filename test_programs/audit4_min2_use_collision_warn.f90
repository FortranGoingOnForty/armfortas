! Two modules make the same bare USE-associated name visible. A reference
! to that name is ambiguous and must be rejected rather than resolved by
! USE statement order.
!
! ERROR_EXPECTED: ambiguous USE-associated reference 'x'
program audit4_min2_use_collision_warn
  use audit4_min2_a
  use audit4_min2_b
  print *, x
end program

module audit4_min2_a
  integer :: x = 1
end module audit4_min2_a

module audit4_min2_b
  integer :: x = 2
end module audit4_min2_b
