program mem2reg_branchy_save
  implicit none

  print *, branchy(1)
  print *, branchy(-1)

contains

  integer function branchy(flag)
    implicit none
    integer, intent(in) :: flag
    integer, save :: slot
    integer :: t

    slot = 7
    if (flag > 0) then
      t = slot
      branchy = t
    else
      t = slot
      branchy = t + 1
    end if
  end function branchy
end program mem2reg_branchy_save

! CHECK: 7
! CHECK: 8
