! audit31 Finding 13: `.and.` / `.or.` on a derived-type LOGICAL
! component tripped the verifier with "operands must be Bool
! (i8 / bool)". The component loads as i8 (the stored byte), and
! coerce_to_type's Int→Bool arm was emitting `int_trunc` which
! produced a value of type i8, not Bool. Rework Int→Bool to emit
! `icmp ne val, 0` so the result IS a Bool. Task #494.
! CHECK: T
program audit31_logical_i8
  implicit none
  type :: rec_t
    logical :: active
  end type
  type(rec_t) :: r
  logical :: ok
  r%active = .true.
  ok = r%active .and. .true.
  print *, ok
end program
