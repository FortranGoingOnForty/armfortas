! CHECK: ok
! IR_CHECK: call @afs_modproc_mvbits_component_class_i64_m_clear_range
! REPRO_CHECK: run
module mvbits_component_class_i64_m
  use iso_fortran_env, only: int32, int64
  implicit none

  integer(int64), parameter :: all_zeros = 0_int64
  integer(int64), parameter :: all_ones = not(all_zeros)

  type :: bitset_64
    integer(int32) :: num_bits = 0_int32
    integer(int64) :: block = 0_int64
  contains
    procedure :: clear_bit
    procedure :: clear_range
    procedure :: set_bit
    procedure :: set_range
    generic :: clear => clear_bit, clear_range
    generic :: set => set_bit, set_range
  end type
contains
  subroutine clear_bit(self, pos)
    class(bitset_64), intent(inout) :: self
    integer(int32), intent(in) :: pos

    self%block = ibclr(self%block, pos)
  end subroutine

  subroutine clear_range(self, start_pos, stop_pos)
    class(bitset_64), intent(inout) :: self
    integer(int32), intent(in) :: start_pos, stop_pos

    call mvbits(0_int64, start_pos, stop_pos - start_pos + 1_int32, self%block, start_pos)
  end subroutine

  subroutine set_bit(self, pos)
    class(bitset_64), intent(inout) :: self
    integer(int32), intent(in) :: pos

    self%block = ibset(self%block, pos)
  end subroutine

  subroutine set_range(self, start_pos, stop_pos)
    class(bitset_64), intent(inout) :: self
    integer(int32), intent(in) :: start_pos, stop_pos

    call mvbits(all_ones, start_pos, stop_pos - start_pos + 1_int32, self%block, start_pos)
  end subroutine
end module

program mvbits_component_class_i64
  use iso_fortran_env, only: int32, int64
  use mvbits_component_class_i64_m
  implicit none

  type(bitset_64) :: set1

  set1%num_bits = 33_int32
  set1%block = int(z'1ffffffff', int64)
  call set1%clear(0_int32)
  call set1%clear(1_int32, 32_int32)

  if (set1%block /= 0_int64) error stop 1
  call set1%set(0_int32)
  call set1%set(1_int32, 32_int32)

  if (set1%block /= int(z'1ffffffff', int64)) error stop 2
  print *, "ok"
end program
