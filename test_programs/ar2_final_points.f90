! CHECK: final 1 10
! CHECK: after_intent_out 20
! CHECK: final 2 30
! CHECK: after_assign 40
! CHECK: final 3 60
! CHECK: final 4 50
! CHECK: after_result 50
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar2_final_points_mod
  implicit none

  integer :: seq = 0

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
      print '(a,1x,i0,1x,i0)', 'final', seq, this%id
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
  use ar2_final_points_mod, only: counted, make_counted, reset_counted
  implicit none

  type(counted) :: a
  type(counted) :: b
  type(counted) :: c
  type(counted) :: rhs

  a%id = 10
  call reset_counted(a)
  print '(a,1x,i0)', 'after_intent_out', a%id

  b%id = 30
  rhs%id = 40
  b = rhs
  print '(a,1x,i0)', 'after_assign', b%id

  c%id = 60
  c = make_counted(50)
  print '(a,1x,i0)', 'after_result', c%id

  a%id = 0
  b%id = 0
  c%id = 0
  rhs%id = 0
end program ar2_final_points
