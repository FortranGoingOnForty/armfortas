! FLAGS: --std=f2023
! CHECK: if=11 12 7
! CHECK: true=5 1 2
! CHECK: false=6 3 4
! CHECK: condarg=42 -1
! CHECK: text=wide
! CHECK: fcstr=5 3
! CHECK: where=1 0 1
! CHECK: forall=1 0 3
program nondefault_logical_conditions
  implicit none

  logical(1) :: flag, mask(3)
  integer :: x, y, left(2), right(2), selected(2), a, b, values(3), i
  character(:), allocatable :: text

  left = [1, 2]
  right = [3, 4]

  flag = .true._1
  if (flag) then
    x = 11
    y = 12
  else
    x = -1
    y = -1
  end if
  if (flag) then
    a = 7
  else
    a = 8
  end if
  print '(a,3(i0,1x))', 'if=', x, y, a

  x = (flag ? 5 : 6)
  selected = (flag ? left : right)
  print '(a,i0,1x,2(i0,1x))', 'true=', x, selected

  flag = .false._1
  x = (flag ? 5 : 6)
  selected = (flag ? left : right)
  print '(a,i0,1x,2(i0,1x))', 'false=', x, selected

  flag = .true._1
  a = -1
  b = -1
  call set_value((flag ? a : b), 42)
  print '(a,2(i0,1x))', 'condarg=', a, b

  text = (flag ? 'wide' : 'z')
  call print_text((flag ? text : 'x'))

  text = f_c_string('ab  ', asis=flag)
  a = len(text)
  flag = .false._1
  text = f_c_string('ab  ', asis=flag)
  b = len(text)
  print '(a,2(i0,1x))', 'fcstr=', a, b

  mask = [.true._1, .false._1, .true._1]
  values = 0
  where (mask) values = 1
  print '(a,3(i0,1x))', 'where=', values

  values = 0
  forall (i = 1:3, mask(i)) values(i) = i
  print '(a,3(i0,1x))', 'forall=', values
contains
  subroutine set_value(value, new_value)
    integer, intent(out) :: value
    integer, intent(in) :: new_value
    value = new_value
  end subroutine set_value

  subroutine print_text(value)
    character(*), intent(in) :: value
    print '(a,a)', 'text=', value
  end subroutine print_text
end program nondefault_logical_conditions
