! CHECK: ok
! IR_CHECK: call @afs_copy_array_data(
! IR_CHECK: call @afs_copy_array_data_no_realloc(
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program sequence_strided_pointer_unit_section
  implicit none
  integer, target :: backing(12)
  integer, pointer :: view(:)
  character(len=2), target :: char_backing(6)
  character(len=2), pointer :: char_view(:)
  integer :: i

  ! Initialize element-by-element so this fixture's copy-helper count only
  ! measures the sequence-association temporaries exercised below.
  do i = 1, size(backing)
    backing(i) = i
  end do
  view => backing(1:12:2)

  call bump(view(:))
  if (any(backing /= [101, 2, 103, 4, 105, 6, 107, 8, 109, 10, 111, 12])) then
    error stop 1
  end if

  call bump(view(::1))
  if (any(backing /= [201, 2, 203, 4, 205, 6, 207, 8, 209, 10, 211, 12])) then
    error stop 2
  end if

  call forward(view)
  if (any(backing /= [301, 2, 303, 4, 305, 6, 307, 8, 309, 10, 311, 12])) then
    error stop 3
  end if

  char_backing = ['a1', 'a2', 'a3', 'a4', 'a5', 'a6']
  char_view => char_backing(1:6:2)
  call mark(char_view(:))
  if (any(char_backing /= ['x1', 'a2', 'x3', 'a4', 'x5', 'a6'])) then
    error stop 4
  end if

  print *, 'ok'

contains
  subroutine forward(values)
    integer, intent(inout) :: values(:)
    call bump(values(:))
  end subroutine forward

  subroutine bump(values)
    integer, intent(inout) :: values(6)
    values = values + 100
  end subroutine bump

  subroutine mark(values)
    character(len=2), intent(inout) :: values(3)
    values = ['x1', 'x3', 'x5']
  end subroutine mark
end program sequence_strided_pointer_unit_section
