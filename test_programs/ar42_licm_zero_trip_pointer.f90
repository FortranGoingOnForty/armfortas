! A zero-trip loop must guard the dereference of a disassociated pointer.
! LICM may hoist the pointer-slot read, but not the potentially faulting read
! through the pointer itself.
module ar42_licm_zero_trip_support
  implicit none

contains

  subroutine sum_captured_optional(n, result, optional_value)
    integer, intent(in) :: n
    integer, intent(out) :: result
    integer, intent(in), optional :: optional_value

    result = 0
    call add_captured_optional()

  contains

    subroutine add_captured_optional()
      integer :: j

      do j = 1, n
        result = result + optional_value
      end do
    end subroutine add_captured_optional
  end subroutine sum_captured_optional
end module ar42_licm_zero_trip_support

program ar42_licm_zero_trip_pointer
  use ar42_licm_zero_trip_support, only: sum_captured_optional
  implicit none
  integer, pointer :: value
  integer :: captured_total, i, optional_total, pointer_total, trip_count, total

  nullify(value)
  trip_count = 0
  total = 0

  do i = 1, trip_count
    total = total + value
  end do

  call sum_absent_optional(0, optional_total)
  call sum_disassociated_pointer(0, pointer_total, value)
  call sum_captured_optional(0, captured_total)
  if (optional_total /= 0 .or. pointer_total /= 0 .or. captured_total /= 0) error stop 2
  print *, total

contains

  recursive subroutine sum_absent_optional(n, result, optional_value)
    integer, intent(in) :: n
    integer, intent(out) :: result
    integer, intent(in), optional :: optional_value
    integer :: j

    if (n < 0) then
      call sum_absent_optional(n, result, optional_value)
      return
    end if

    result = 0
    do j = 1, n
      result = result + optional_value
    end do
  end subroutine sum_absent_optional

  subroutine sum_disassociated_pointer(n, result, pointer_value)
    integer, intent(in) :: n
    integer, intent(out) :: result
    integer, intent(in), pointer :: pointer_value
    integer :: j

    result = 0
    do j = 1, n
      result = result + pointer_value
    end do
  end subroutine sum_disassociated_pointer
end program ar42_licm_zero_trip_pointer
! CHECK: 0
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
