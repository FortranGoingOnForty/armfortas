! Module allocatable array allocated in a module procedure and
! read back in the main program.  Previously insert_implicit_dealloc
! freed module globals at procedure return because it didn't
! distinguish module-level storage from local allocatables.
module state
  implicit none
  integer, allocatable :: buf(:)
contains
  subroutine init()
    allocate(buf(3))
    buf(1) = 10; buf(2) = 20; buf(3) = 30
  end subroutine
  subroutine double_it()
    integer :: i
    do i = 1, size(buf)
      buf(i) = buf(i) * 2
    end do
  end subroutine
end module
program module_allocatable_lifecycle
  use state
  call init()
  print *, buf(1), buf(2), buf(3)
  call double_it()
  print *, buf(1), buf(2), buf(3)
end program
! CHECK: 10 20 30
! CHECK: 20 40 60
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
