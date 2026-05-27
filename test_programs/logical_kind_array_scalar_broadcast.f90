! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program logical_kind_array_scalar_broadcast
  use, intrinsic :: iso_fortran_env, only: int8, int16, int64
  implicit none

  logical(int8) :: a8(64) = .true.
  logical(int16) :: a16(31) = .false.
  logical(int64) :: a64(33) = .true.
  logical :: default_logical(8) = .true.

  if (.not. all(a8)) error stop 1
  if (any(a16)) error stop 2
  if (.not. all(a64)) error stop 3
  if (.not. all(default_logical)) error stop 4

  call check_int8(a8)
  call check_int16(a16)
  call check_int64(a64)

  write(*, "(a)") "ok"

contains
  subroutine check_int8(values)
    logical(int8), intent(in) :: values(:)

    if (size(values) /= 64) error stop 11
    if (.not. all(values)) error stop 12
  end subroutine check_int8

  subroutine check_int16(values)
    logical(int16), intent(in) :: values(:)

    if (size(values) /= 31) error stop 21
    if (any(values)) error stop 22
  end subroutine check_int16

  subroutine check_int64(values)
    logical(int64), intent(in) :: values(:)

    if (size(values) /= 33) error stop 31
    if (.not. all(values)) error stop 32
  end subroutine check_int64
end program logical_kind_array_scalar_broadcast
