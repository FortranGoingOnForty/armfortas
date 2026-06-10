! l01: F2023 8.9 allows a PUBLIC namelist group to contain a PRIVATE
! group object (F2018 C8107 forbade it). armfortas never enforced the
! old constraint; this fixture keeps the relaxed form legal.
! FLAGS: --std=f2023
! CHECK: 5
module nml_mod
  implicit none
  private
  integer, public :: visible = 0
  integer :: hidden = 5
  namelist /grp/ visible, hidden
  public :: grp, show
contains
  subroutine show()
    print *, hidden
  end subroutine show
end module nml_mod

program l01_namelist_private_member
  use nml_mod, only: show
  implicit none
  call show()
end program l01_namelist_private_member
