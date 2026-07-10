program ar4_iospec_contained_call
  implicit none

  integer :: unit
  logical :: exists
  character(len=32) :: path

  path = 'ar4_iospec_' // basename(7) // '.dat'

  open(newunit=unit, file='ar4_iospec_' // basename(7) // '.dat', status='replace', action='write')
  write(unit, '(a)') 'ok'
  close(unit)

  inquire(file='ar4_iospec_' // basename(7) // '.dat', exist=exists)
  print *, 'exists', exists
  ! CHECK: exists T

  open(newunit=unit, file=path, status='old', action='read')
  close(unit, status='delete')

contains
  function basename(n) result(out)
    integer, intent(in) :: n
    character(len=3) :: out

    write(out, '(i3.3)') n
  end function
end program
