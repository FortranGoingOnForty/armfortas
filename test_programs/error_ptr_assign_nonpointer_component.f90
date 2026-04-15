! Pointer assignment into a non-POINTER derived-type component.
! FieldLayout now carries per-field attributes so the validator can
! distinguish `obj%ptr_comp => x` (legal) from `obj%plain_comp => x`
! (error) instead of short-circuiting to the base variable.
! ERROR_EXPECTED: target component 'plain' must have pointer attribute
program t
  implicit none
  type :: rec_t
    integer, pointer :: ptr => null()
    integer :: plain = 0
  end type
  integer, target :: val = 7
  type(rec_t) :: r
  r%plain => val          ! plain has no pointer attribute
end program
