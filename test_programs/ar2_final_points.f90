! FILE_CHECK: ar2_final_points.log => final 1 10
! FILE_CHECK: ar2_final_points.log => after_intent_out 20
! FILE_CHECK: ar2_final_points.log => final 2 30
! FILE_CHECK: ar2_final_points.log => after_assign 40
! FILE_CHECK: ar2_final_points.log => final 3 60
! FILE_CHECK: ar2_final_points.log => final 4 50
! FILE_CHECK: ar2_final_points.log => after_result 50
! FILE_LINE_COUNT: ar2_final_points.log => 7
! FILE_SET_EXACT: ar2_final_points.log
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar2_final_points_mod
  implicit none

  integer :: seq = 0
  integer, parameter :: log_unit = 77

  type :: counted
    integer :: id = 0
  contains
    final :: finish_counted
  end type counted
contains
  subroutine finish_counted(this)
    type(counted), intent(inout) :: this

    if (this%id /= 0) then
      seq = seq + 1
      write(log_unit, '(a,1x,i0,1x,i0)') 'final', seq, this%id
    end if
  end subroutine finish_counted

  subroutine reset_counted(this)
    type(counted), intent(out) :: this

    this%id = 20
  end subroutine reset_counted

  function make_counted(id) result(res)
    integer, intent(in) :: id
    type(counted) :: res

    res%id = id
  end function make_counted
end module ar2_final_points_mod

program ar2_final_points
  use ar2_final_points_mod, only: counted, log_unit, make_counted, reset_counted
  implicit none

  type(counted) :: a
  type(counted) :: b
  type(counted) :: c
  type(counted) :: rhs

  open(unit=log_unit, file='ar2_final_points.log', status='replace', action='write')

  a%id = 10
  call reset_counted(a)
  write(log_unit, '(a,1x,i0)') 'after_intent_out', a%id

  b%id = 30
  rhs%id = 40
  b = rhs
  write(log_unit, '(a,1x,i0)') 'after_assign', b%id

  c%id = 60
  c = make_counted(50)
  write(log_unit, '(a,1x,i0)') 'after_result', c%id
end program ar2_final_points
