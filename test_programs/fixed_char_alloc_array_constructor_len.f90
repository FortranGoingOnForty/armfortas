! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
program fixed_char_alloc_array_constructor_len
  implicit none

  character(len=4), allocatable :: words(:)

  allocate(words(0))
  words = ['#1', '#2', '#3', '#4']

  if (size(words) /= 4) error stop 1
  if (words(1) /= '#1  ') error stop 2
  if (words(4) /= '#4  ') error stop 3

  call check(words)
  print *, "ok"

contains

  subroutine check(items)
    character(len=*), intent(in) :: items(:)

    if (len(items(1)) /= 4) error stop 4
    if (items(2) /= '#2  ') error stop 5
  end subroutine check

end program fixed_char_alloc_array_constructor_len
