! ERROR_EXPECTED: internal NAMELIST WRITE is not implemented
program ar4_namelist_internal_write_rejected
  implicit none

  character(len=128) :: buf
  integer :: n = 1

  namelist /cfg/ n

  write(buf, nml=cfg)
end program
