! CHECK: integer 42
! CHECK: real
! CHECK: character abc
! CHECK: derived 77
! CHECK: default
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar2_select_type_unlimited_m
  implicit none

  type :: payload
    integer :: n = 0
  end type payload
end module ar2_select_type_unlimited_m

program ar2_select_type_unlimited
  use ar2_select_type_unlimited_m
  implicit none

  class(*), allocatable :: box
  type(payload) :: p

  allocate(box, source=42)
  select type (box)
  type is (integer)
    print "(a,i0)", "integer ", box
  class default
    print "(a)", "integer default"
  end select
  deallocate(box)

  allocate(box, source=1.5)
  select type (box)
  type is (real)
    if (abs(box - 1.5) > 0.001) error stop 1
    print "(a)", "real"
  class default
    print "(a)", "real default"
  end select
  deallocate(box)

  allocate(box, source="abc")
  select type (box)
  type is (character(len=*))
    print "(a,a)", "character ", box
  class default
    print "(a)", "character default"
  end select
  deallocate(box)

  p%n = 77
  allocate(box, source=p)
  select type (box)
  type is (payload)
    print "(a,i0)", "derived ", box%n
  class default
    print "(a)", "derived default"
  end select
  deallocate(box)

  allocate(box, source=.true.)
  select type (box)
  type is (integer)
    print "(a)", "wrong"
  class default
    print "(a)", "default"
  end select
end program ar2_select_type_unlimited
