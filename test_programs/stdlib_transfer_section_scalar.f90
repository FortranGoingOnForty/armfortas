! CHECK: ok
! REPRO_CHECK: run
program stdlib_transfer_section_scalar
  use iso_fortran_env, only: int8, int16
  implicit none

  integer(int8) :: key(300)
  integer :: i

  do i = 1, 300
    key(i) = int(iand(i, 255), int8)
  end do

  call probe(key(1:2))

contains
  subroutine probe(key)
    integer(int8), intent(in) :: key(0:)

    if (readle16(key) /= int(z'0201', int16)) error stop 1

    print *, 'ok'

  contains
    pure function readle16(p) result(v)
      integer(int8), intent(in) :: p(:)
      integer(int16) :: v

      v = transfer(p(1:2), 0_int16)
    end function readle16
  end subroutine probe
end program stdlib_transfer_section_scalar
