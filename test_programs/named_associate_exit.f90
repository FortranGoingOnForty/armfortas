! CHECK: result=41 7 1
program named_associate_exit
  implicit none
  integer :: target, value, after

  target = 0
  value = 7
  after = 0

OuterScope: associate (value => target)
  InnerLoop: do
    value = 41
    exit oUtErScOpE
  end do InnerLoop
  value = 99
end associate OUTERSCOPE

  after = after + 1
  write (*, '(a,i0,1x,i0,1x,i0)') 'result=', target, value, after
end program named_associate_exit
