! CHECK: ok
! IR_CHECK: call @afs_dealloc_string
! IR_CHECK: derived_dealloc_elem_body
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module derived_alloc_component_array_deallocate_m
  implicit none
  character(len=64), parameter :: payload = 'abc'

  type :: string_t
    character(len=:), allocatable :: str
  end type string_t
contains
  subroutine churn()
    type(string_t), allocatable :: values(:)
    integer :: i

    allocate(values(8))
    do i = 1, size(values)
      values(i)%str = payload
    end do

    if (len(values(8)%str) /= 64) error stop 1
    if (values(8)%str(1:3) /= 'abc') error stop 2

    deallocate(values)
  end subroutine churn
end module derived_alloc_component_array_deallocate_m

program derived_alloc_component_array_deallocate
  use derived_alloc_component_array_deallocate_m
  implicit none
  integer :: i

  do i = 1, 4
    call churn()
  end do

  print *, 'ok'
end program derived_alloc_component_array_deallocate
