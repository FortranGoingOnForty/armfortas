! Scalar POINTER associated with TARGET storage.
!
! Previously `integer, pointer :: p` lowered as a plain `alloca i32`
! with no indirection, `Stmt::PointerAssignment` (`=>`) fell through
! the lower_stmt `_ => {}` arm without emitting anything, and `p = v`
! stored directly into p's alloca instead of through it.  Reading p
! then returned the value of the local slot (uninitialised), not the
! target — and ASSOCIATED/updates were silent no-ops.
program pointer_scalar_target
  implicit none
  integer, target :: x
  integer, target :: y
  integer, pointer :: p1, p2

  x = 10
  y = 20

  p1 => x
  p2 => x

  ! Dereference-on-write: `p1 = 100` must store through p1's slot,
  ! not overwrite p1 itself.
  p1 = 100
  print *, p2, x    ! both see 100

  ! Pointer-to-pointer reassociation.
  p2 => y
  print *, p1, p2   ! 100 20

  ! Chained pointer-from-pointer.
  p1 => p2
  print *, p1, p2   ! 20 20
end program pointer_scalar_target
! CHECK: 100 100
! CHECK: 100 20
! CHECK: 20 20
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
