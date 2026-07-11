! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! CHECK: local=5 T
! CHECK: short=3 T
! CHECK: padded=5 T
! CHECK: truncated=3 T
! CHECK: component=4 T
! CHECK: reassigned=4 T

program allocate_fixed_char_source
  implicit none

  type :: box_t
    character(4), allocatable :: text
  end type box_t

  character(5), allocatable :: local
  character(3), allocatable :: short
  character(5), allocatable :: padded
  character(3), allocatable :: truncated
  type(box_t) :: box

  allocate(local, source='abcde')
  allocate(short, source='xyz')
  allocate(padded, source='xy')
  allocate(truncated, source='abcde')
  allocate(box%text, source='xy')

  print '(a,i0,1x,l1)', 'local=', len(local), local == 'abcde'
  print '(a,i0,1x,l1)', 'short=', len(short), short == 'xyz'
  print '(a,i0,1x,l1)', 'padded=', len(padded), padded == 'xy   '
  print '(a,i0,1x,l1)', 'truncated=', len(truncated), truncated == 'abc'
  print '(a,i0,1x,l1)', 'component=', len(box%text), box%text == 'xy  '

  if (local /= 'abcde') error stop 1
  if (short /= 'xyz') error stop 2
  if (padded /= 'xy   ') error stop 3
  if (truncated /= 'abc') error stop 4
  if (box%text /= 'xy  ') error stop 5

  box%text = 'q'
  print '(a,i0,1x,l1)', 'reassigned=', len(box%text), box%text == 'q   '
  call check_component(box%text)

contains

  subroutine check_component(value)
    character(*), intent(in) :: value

    if (len(value) /= 4) error stop 6
    if (value /= 'q   ') error stop 7
  end subroutine check_component
end program allocate_fixed_char_source
