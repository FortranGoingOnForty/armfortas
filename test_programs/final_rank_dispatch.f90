! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! MULTIFILE_LINK: final_rank_dispatch_mod.f90 final_rank_dispatch_main.f90
! CHECK: scalar=1
! CHECK: rank1=2
! CHECK: elements=12
! CHECK: strided=3 9

!--- file: final_rank_dispatch_mod.f90
module final_rank_dispatch_mod
  implicit none

  integer :: scalar_calls = 0
  integer :: rank_one_calls = 0
  integer :: rank_one_elements = 0
  integer :: strided_calls = 0
  integer :: strided_sum = 0

  type :: counted
    integer :: value = 0
  contains
    final :: finish_scalar, finish_rank_one
  end type counted

  type :: scalar_counted
    integer :: value = 0
  contains
    final :: finish_strided_scalar
  end type scalar_counted

contains

  subroutine finish_scalar(value)
    type(counted), intent(inout) :: value
    scalar_calls = scalar_calls + 1
    rank_one_elements = rank_one_elements + value%value
  end subroutine finish_scalar

  subroutine finish_rank_one(values)
    type(counted), intent(inout) :: values(:)
    rank_one_calls = rank_one_calls + 1
    rank_one_elements = rank_one_elements + size(values)
  end subroutine finish_rank_one

  subroutine finalize_fixed_array()
    type(counted) :: values(3)
    values%value = 10
  end subroutine finalize_fixed_array

  subroutine finalize_allocatable_array()
    type(counted), allocatable :: values(:)
    allocate(values(2))
    values%value = 20
    deallocate(values)
  end subroutine finalize_allocatable_array

  subroutine finalize_scalar()
    type(counted) :: value
    value%value = 7
  end subroutine finalize_scalar

  subroutine finish_strided_scalar(value)
    type(scalar_counted), intent(inout) :: value
    strided_calls = strided_calls + 1
    strided_sum = strided_sum + value%value
  end subroutine finish_strided_scalar

  subroutine clear_strided(values)
    type(scalar_counted), intent(out) :: values(:)
  end subroutine clear_strided

end module final_rank_dispatch_mod

!--- file: final_rank_dispatch_main.f90
program final_rank_dispatch
  use final_rank_dispatch_mod
  implicit none
  type(scalar_counted) :: strided(5)

  strided%value = [1, 2, 3, 4, 5]

  call finalize_scalar()
  call finalize_fixed_array()
  call finalize_allocatable_array()
  call clear_strided(strided(1:5:2))

  print '(a,i0)', 'scalar=', scalar_calls
  print '(a,i0)', 'rank1=', rank_one_calls
  print '(a,i0)', 'elements=', rank_one_elements
  print '(a,i0,1x,i0)', 'strided=', strided_calls, strided_sum

  if (scalar_calls /= 1) error stop 1
  if (rank_one_calls /= 2) error stop 2
  if (rank_one_elements /= 12) error stop 3
  if (strided_calls /= 3 .or. strided_sum /= 9) error stop 4
end program final_rank_dispatch
